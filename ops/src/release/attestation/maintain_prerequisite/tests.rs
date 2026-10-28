//! Inert contract and real-local-FD regressions. No namespace/launcher proof.
use super::*;
use protocol::{Clock, Frame, Kind, Phase};
fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}
#[test]
fn only_fixed_trial_and_time_profile_enter_run() {
    for trial in [
        "normal",
        "cancel-owned-launcher",
        "timeout-stopped-launcher",
    ] {
        assert!(
            Options::parse(&args(&[
                "run",
                "--trial",
                trial,
                "--timeout-seconds",
                "1050",
                "--finalization-seconds",
                "60"
            ]))
            .is_ok()
        );
    }
    for words in [
        vec![
            "run",
            "--trial",
            "shell",
            "--timeout-seconds",
            "1050",
            "--finalization-seconds",
            "60",
        ],
        vec![
            "run",
            "--trial",
            "normal",
            "--timeout-seconds",
            "1200",
            "--finalization-seconds",
            "60",
        ],
        vec![
            "run",
            "--trial",
            "normal",
            "--timeout-seconds",
            "1050",
            "--finalization-seconds",
            "0",
        ],
    ] {
        assert!(Options::parse(&args(&words)).is_err());
    }
}
#[test]
fn internal_init_roundtrip_keeps_trial_nonce_and_same_clock() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let words = vec![
        "init".to_owned(),
        "--trial".into(),
        "normal".into(),
        "--nonce".into(),
        nonce.clone(),
        "--origin-monotonic-ns".into(),
        clock.origin.to_string(),
        "--body-deadline-monotonic-ns".into(),
        clock.body.to_string(),
        "--deadline-monotonic-ns".into(),
        clock.end.to_string(),
    ];
    let words: Vec<OsString> = words.into_iter().map(OsString::from).collect();
    match Options::parse(&words).unwrap() {
        Options::Init {
            trial,
            nonce: actual,
            clock: actual_clock,
        } => {
            assert_eq!(trial, Trial::Normal);
            assert_eq!(actual, nonce);
            assert_eq!(actual_clock, clock);
        }
        Options::Run(_) | Options::NormalInit { .. } => panic!("wrong fixed role"),
    }
    assert!(matches!(
        Options::parse(&args(&[
            "run",
            "--trial",
            "normal",
            "--timeout-seconds",
            "1050",
            "--finalization-seconds",
            "60"
        ])),
        Ok(Options::Run(Trial::Normal))
    ));
}
#[test]
fn absolute_init_interval_cannot_reset_or_extend() {
    let clock = Clock::new(1_000_000_000).unwrap();
    assert_eq!(clock.end - clock.origin, WHOLE * 1_000_000_000);
    assert_eq!(clock.end - clock.body, FINAL * 1_000_000_000);
    assert!(Clock::parse("1", "990000000001", "1050000000001").is_ok());
    for values in [
        ("0", "990000000000", "1050000000000"),
        ("01", "990000000001", "1050000000001"),
        ("1", "990000000002", "1050000000001"),
        ("1", "990000000001", "1050000000002"),
    ] {
        assert!(Clock::parse(values.0, values.1, values.2).is_err());
    }
}

#[test]
fn inherited_normal_init_keeps_original_origin_without_widening_fixed_entry() {
    let origin = 1_000_000_000u64;
    let end = origin + 1500 * 1_000_000_000;
    let body = end - FINAL * 1_000_000_000;
    let nonce = "a".repeat(32);
    let words = args(&[
        "normal-init",
        "--nonce",
        &nonce,
        "--origin-monotonic-ns",
        &origin.to_string(),
        "--body-deadline-monotonic-ns",
        &body.to_string(),
        "--deadline-monotonic-ns",
        &end.to_string(),
    ]);
    match Options::parse(&words).unwrap() {
        Options::NormalInit {
            nonce: actual,
            clock,
        } => {
            assert_eq!(actual, nonce);
            assert_eq!(clock.origin, origin);
            assert_eq!(clock.body, body);
            assert_eq!(clock.end, end);
        }
        _ => panic!("wrong inherited normal role"),
    }
    assert!(Clock::parse(&origin.to_string(), &body.to_string(), &end.to_string()).is_err());
    let mut with_trial = words;
    with_trial.extend(args(&["--trial", "cancel-owned-launcher"]));
    assert!(Options::parse(&with_trial).is_err());
}

#[test]
fn inherited_clock_refuses_noncanonical_or_unbounded_parent_intervals() {
    assert!(Clock::parse_inherited("1", "1740000000001", "1800000000001").is_ok());
    for (origin, body, end) in [
        ("0", "1740000000000", "1800000000000"),
        ("01", "1740000000001", "1800000000001"),
        ("1", "1740000000002", "1800000000002"),
        ("1", "1740000000002", "1800000000001"),
        ("1", "1", "60000000001"),
        ("2", "1", "60000000001"),
        ("18446744073709551616", "1740000000001", "1800000000001"),
    ] {
        assert!(Clock::parse_inherited(origin, body, end).is_err());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_clock_carries_preparation_in_the_original_parent_interval() {
    use std::time::{Duration, Instant};
    let started = Instant::now()
        .checked_sub(Duration::from_secs(100))
        .unwrap();
    let mut owner =
        Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
    let end = started + Duration::from_secs(1400);
    let body = end - Duration::from_secs(FINAL);
    let clock = Clock::inherited(&mut owner, body, end).unwrap();
    let interval = clock.end - clock.origin;
    let work = clock.body - clock.origin;
    let original_end = owner.end();
    let original_body = owner.body_end();
    let checked = clock.require_work(&mut owner);
    let finished = owner.finish();
    assert_eq!(interval, 1400 * 1_000_000_000);
    assert_eq!(work, 1340 * 1_000_000_000);
    assert_eq!(original_end, started + Duration::from_secs(1800));
    assert_eq!(original_body, started + Duration::from_secs(1740));
    assert_eq!(checked, Ok(()));
    assert_eq!(finished, Ok(()));
}

#[cfg(target_os = "linux")]
#[test]
fn borrowed_normal_refuses_insufficient_debt_before_any_native_effect() {
    use std::time::{Duration, Instant};
    for preparation in [Duration::ZERO, Duration::from_secs(100)] {
        let started = Instant::now().checked_sub(preparation).unwrap();
        let mut owner =
            Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
        let original_end = owner.end();
        let original_body = owner.body_end();
        let ceiling = owner.now() + Duration::from_secs(KNOWN + FINAL);
        let mut observed = None;
        let result = launch_normal(&mut owner, ceiling, &mut observed);
        let retained_end = owner.end();
        let retained_body = owner.body_end();
        let first = owner.error();
        let finished = owner.finish();
        assert!(matches!(result, Err(Cause::Deadline)));
        assert!(observed.is_none());
        assert_eq!(retained_end, original_end);
        assert_eq!(retained_body, original_body);
        assert_eq!(first, Some(Cause::Deadline));
        assert_eq!(finished, Err(Cause::Deadline));
    }
}
#[test]
fn frame_binds_nonce_trial_and_original_interval() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let frame = Frame::new(Kind::Go, &nonce, Trial::Normal, clock);
    let raw = frame.bytes().unwrap();
    assert!(Frame::decode(&raw, Kind::Go, &nonce, Trial::Normal, clock).is_ok());
    assert!(Frame::decode(&raw, Kind::Go, &"b".repeat(32), Trial::Normal, clock).is_err());
    assert!(Frame::decode(&raw, Kind::Go, &nonce, Trial::CancelOwnedLauncher, clock).is_err());
    assert!(
        Frame::decode(
            &raw,
            Kind::Go,
            &nonce,
            Trial::Normal,
            Clock::new(2).unwrap()
        )
        .is_err()
    );
}
#[test]
fn unknown_secret_or_duplicate_fields_never_enter_frames() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let raw = Frame::new(Kind::Go, &nonce, Trial::Normal, clock)
        .bytes()
        .unwrap();
    for suffix in [",\"opaqueSecret\":\"private\"}", ",\"kind\":\"go\"}"] {
        let mut changed = raw[..raw.len() - 1].to_vec();
        changed.extend_from_slice(suffix.as_bytes());
        assert!(Frame::decode(&changed, Kind::Go, &nonce, Trial::Normal, clock).is_err());
    }
}
#[test]
fn nullable_frame_fields_are_present_not_implicit_defaults() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let raw = Frame::new(Kind::Go, &nonce, Trial::Normal, clock)
        .bytes()
        .unwrap();
    for field in ["facts", "failure", "phaseDeadlineNs"] {
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            Frame::decode(
                &serde_json::to_vec(&value).unwrap(),
                Kind::Go,
                &nonce,
                Trial::Normal,
                clock
            )
            .is_err()
        );
    }
}
#[test]
fn only_result_frames_carry_closed_facts() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let mut frame = Frame::new(Kind::Ready, &nonce, Trial::Normal, clock);
    frame.facts = Some(protocol::Facts::default());
    assert!(
        Frame::decode(
            &frame.bytes().unwrap(),
            Kind::Ready,
            &nonce,
            Trial::Normal,
            clock
        )
        .is_err()
    );
    frame.kind = Kind::Result;
    frame.phase = Phase::Result;
    frame.phase_deadline_ns = Some(clock.end);
    assert!(
        Frame::decode(
            &frame.bytes().unwrap(),
            Kind::Result,
            &nonce,
            Trial::Normal,
            clock
        )
        .is_ok()
    );
}
#[test]
fn peer_facts_reject_text_status_and_vector_escape_without_erasing_refusal_counts() {
    let clock = Clock::new(1).unwrap();
    let nonce = "a".repeat(32);
    let mut frame = Frame::new(Kind::Result, &nonce, Trial::Normal, clock);
    frame.phase_deadline_ns = Some(clock.end);
    frame.facts = Some(protocol::Facts::default());
    for text in ["private/path", &"A".repeat(64), &"f".repeat(63)] {
        let mut changed = frame.clone();
        changed.facts.as_mut().unwrap().stdout_sha256 = Some(text.to_owned());
        assert!(
            Frame::decode(
                &changed.bytes().unwrap(),
                Kind::Result,
                &nonce,
                Trial::Normal,
                clock
            )
            .is_err()
        );
    }
    for status in [-128, 256] {
        let mut changed = frame.clone();
        changed.facts.as_mut().unwrap().launcher_raw_status = Some(status);
        assert!(
            Frame::decode(
                &changed.bytes().unwrap(),
                Kind::Result,
                &nonce,
                Trial::Normal,
                clock
            )
            .is_err()
        );
    }
    let mut changed = frame.clone();
    changed.facts.as_mut().unwrap().adopted_raw_statuses = vec![0; 257];
    assert!(
        Frame::decode(
            &changed.bytes().unwrap(),
            Kind::Result,
            &nonce,
            Trial::Normal,
            clock
        )
        .is_err()
    );
    frame.failure = Some(protocol::Failure::Command);
    let facts = frame.facts.as_mut().unwrap();
    facts.adopted_reaped = 257;
    facts.adopted_raw_statuses = vec![0; 256];
    facts.stdout_bytes = MAX_STREAM + 1;
    let decoded = Frame::decode(
        &frame.bytes().unwrap(),
        Kind::Result,
        &nonce,
        Trial::Normal,
        clock,
    )
    .unwrap();
    assert_eq!(decoded.facts.unwrap().stdout_bytes, MAX_STREAM + 1);
}
#[test]
fn closed_wire_failure_roundtrip_preserves_original_cause() {
    for cause in [
        Cause::Input,
        Cause::Policy,
        Cause::Changed,
        Cause::Io,
        Cause::Close,
        Cause::Deadline,
        Cause::Interrupt,
        Cause::Terminate,
        Cause::Bounds,
        Cause::Command,
    ] {
        let clock = Clock::new(1).unwrap();
        let nonce = "a".repeat(32);
        let mut frame = Frame::new(Kind::Result, &nonce, Trial::Normal, clock);
        frame.phase_deadline_ns = Some(clock.end);
        frame.facts = Some(protocol::Facts::default());
        frame.failure = Some(cause.into());
        let decoded = Frame::decode(
            &frame.bytes().unwrap(),
            Kind::Result,
            &nonce,
            Trial::Normal,
            clock,
        )
        .unwrap();
        let actual: Cause = decoded.failure.unwrap().into();
        assert_eq!(actual, cause);
    }
}
#[cfg(target_os = "linux")]
mod descriptors {
    use super::*;
    use std::{
        os::fd::{AsFd, OwnedFd},
        time::{Duration, Instant},
    };
    struct Slot {
        fd: Option<OwnedFd>,
        flags: Option<rustix::fs::OFlags>,
    }
    struct Fixture {
        started: Instant,
        owner: Option<Owner>,
        slots: Vec<Slot>,
        captures: Vec<process::Capture>,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_start(Instant::now())
        }
        fn with_start(started: Instant) -> Self {
            Self {
                started,
                owner: Some(
                    Owner::new(
                        started,
                        Duration::from_secs(WHOLE),
                        Duration::from_secs(FINAL),
                    )
                    .unwrap(),
                ),
                slots: Vec::new(),
                captures: Vec::new(),
            }
        }
        fn push(&mut self, fd: OwnedFd) -> usize {
            let index = self.slots.len();
            self.slots.push(Slot {
                fd: Some(fd),
                flags: None,
            });
            index
        }
        fn prepare(&mut self) -> Result<()> {
            for slot in &mut self.slots {
                slot.flags = Some(
                    rustix::fs::fcntl_getfl(slot.fd.as_ref().ok_or(Cause::Input)?)
                        .map_err(|_| Cause::Io)?,
                );
            }
            Ok(())
        }
        fn parts(&mut self) -> (&mut Owner, &mut Vec<Slot>, &mut Vec<process::Capture>) {
            (
                self.owner
                    .as_mut()
                    .expect("fixture owner remains until finish"),
                &mut self.slots,
                &mut self.captures,
            )
        }
        fn finish(&mut self) -> Option<Cause> {
            let mut owner = self.owner.take()?;
            for value in &mut self.captures {
                value.finish(&mut owner);
            }
            for slot in &mut self.slots {
                if let Some(fd) = slot.fd.take() {
                    if let Some(flags) = slot.flags
                        && rustix::fs::fcntl_setfl(&fd, flags).is_err()
                    {
                        owner.record(Cause::Close);
                    }
                    let _ = identity::close(&mut owner, fd);
                }
            }
            let first = owner.error();
            first.or(owner.finish().err())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.finish();
        }
    }
    fn fd(slots: &[Slot], index: usize) -> &OwnedFd {
        slots[index]
            .fd
            .as_ref()
            .expect("admitted original remains held")
    }
    fn pair() -> (OwnedFd, OwnedFd) {
        rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap()
    }
    fn null() -> OwnedFd {
        rustix::fs::open(
            "/dev/null",
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap()
    }
    fn close_events() -> Vec<bool> {
        identity::CLOSED.with(|v| std::mem::take(&mut *v.borrow_mut()))
    }
    #[test]
    fn exact_observed_image_mismatch_survives_actual_close_fault() {
        let mut fixture = Fixture::new();
        let index = fixture.push(null());
        fixture.prepare().unwrap();
        let _ = close_events();
        let (result, primary) = {
            let (owner, slots, _) = fixture.parts();
            let original = slots[index].fd.take().unwrap();
            identity::CLOSE_FAULT.with(|v| v.set(true));
            let result = process::image_identity(original, (0, 0), owner);
            (result, owner.error())
        };
        let closed = close_events();
        let _ = fixture.finish();
        assert_eq!(result, Err(Cause::Changed));
        assert_eq!(primary, Some(Cause::Changed));
        assert_eq!(closed, vec![true]);
    }
    #[test]
    fn late_actual_eof_refuses_before_secondary_close_error() {
        let mut fixture = Fixture::new();
        let index = fixture.push(null());
        fixture.prepare().unwrap();
        let _ = close_events();
        let (result, primary) = {
            let (owner, slots, _) = fixture.parts();
            identity::READ_EXPIRE.with(|v| v.set(true));
            let result = identity::read_fd(fd(slots, index), owner);
            identity::CLOSE_FAULT.with(|v| v.set(true));
            let _ = identity::close(owner, slots[index].fd.take().unwrap());
            (result, owner.error())
        };
        let closed = close_events();
        let _ = fixture.finish();
        assert_eq!(result, Err(Cause::Deadline));
        assert_eq!(primary, Some(Cause::Deadline));
        assert_eq!(closed, vec![true]);
    }
    #[test]
    fn passcred_enabled_inner_channel_is_refused_before_receive() {
        let mut fixture = Fixture::new();
        let (left, right) = pair();
        let _left = fixture.push(left);
        let right = fixture.push(right);
        fixture.prepare().unwrap();
        let refused = {
            let (owner, slots, _) = fixture.parts();
            rustix::net::sockopt::set_socket_passcred(fd(slots, right), true).unwrap();
            process::Channel::new(fd(slots, right).as_fd(), false, owner).is_err()
        };
        let _ = fixture.finish();
        assert!(refused);
    }
    #[test]
    fn packet_completed_after_phase_end_closes_received_rights() {
        for preparation in [Duration::ZERO, Duration::from_secs(10)] {
            let started = Instant::now().checked_sub(preparation).unwrap();
            let mut fixture = Fixture::with_start(started);
            let (left, right) = pair();
            let left = fixture.push(left);
            let right = fixture.push(right);
            let a = fixture.push(null());
            let b = fixture.push(null());
            fixture.prepare().unwrap();
            let started = fixture.started;
            let (refused, primary, received_closed) = {
                let (owner, slots, _) = fixture.parts();
                let sender = process::Channel::new(fd(slots, left).as_fd(), false, owner).unwrap();
                let receiver =
                    process::Channel::new(fd(slots, right).as_fd(), false, owner).unwrap();
                let frame = Frame::new(
                    Kind::Go,
                    &"a".repeat(32),
                    Trial::Normal,
                    Clock::new(1).unwrap(),
                );
                let end = owner.now() + Duration::from_secs(1);
                let late = end.duration_since(started) + Duration::from_nanos(1);
                sender
                    .send(
                        &frame,
                        &[fd(slots, a).as_fd(), fd(slots, b).as_fd()],
                        owner,
                        end,
                        false,
                    )
                    .unwrap();
                let _ = close_events();
                process::RECV_ELAPSED.with(|v| v.set(Some(late)));
                let result = receiver.receive(owner, end, false);
                let received_closed = close_events();
                let primary = owner.error();
                let _ = receiver.finish(owner);
                let _ = sender.finish(owner);
                (
                    matches!(result, Err(Cause::Deadline)),
                    primary,
                    received_closed,
                )
            };
            let _ = fixture.finish();
            assert!(refused);
            assert_eq!(primary, Some(Cause::Deadline));
            assert_eq!(received_closed, vec![true, true]);
        }
    }
    #[test]
    fn actual_capture_eof_latches_end_before_close_fault() {
        let mut fixture = Fixture::new();
        let index = fixture.push(null());
        fixture.prepare().unwrap();
        {
            let (owner, slots, captures) = fixture.parts();
            let original = slots[index].fd.take().unwrap();
            captures.push(process::Capture::new(original, owner).unwrap());
        }
        let _ = close_events();
        let (result, primary, eof) = {
            let (owner, _, captures) = fixture.parts();
            process::CAPTURE_ELAPSED.with(|v| v.set(Some(Duration::from_secs(WHOLE))));
            let result = captures[0].pump(owner, Window::Final);
            identity::CLOSE_FAULT.with(|v| v.set(true));
            captures[0].finish(owner);
            (result, owner.error(), captures[0].eof)
        };
        let closed = close_events();
        let _ = fixture.finish();
        assert_eq!(result, Err(Cause::Deadline));
        assert_eq!(primary, Some(Cause::Deadline));
        assert!(eof);
        assert_eq!(closed, vec![true]);
    }
    #[test]
    fn consumed_late_data_stays_counted_and_never_yields_gap_digest() {
        let mut fixture = Fixture::new();
        let (left, right) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let left = fixture.push(left);
        let right = fixture.push(right);
        fixture.prepare().unwrap();
        let (result, primary, count, digest, eof) = {
            let (owner, slots, captures) = fixture.parts();
            rustix::io::write(fd(slots, left), b"abc").unwrap();
            let _ = identity::close(owner, slots[left].fd.take().unwrap());
            let original = slots[right].fd.take().unwrap();
            captures.push(process::Capture::new(original, owner).unwrap());
            process::CAPTURE_ELAPSED.with(|v| v.set(Some(Duration::from_secs(WHOLE - FINAL))));
            let result = captures[0].pump(owner, Window::Body);
            let primary = owner.error();
            let _ = captures[0].pump(owner, Window::Final);
            (
                result,
                primary,
                captures[0].bytes,
                captures[0].digest(),
                captures[0].eof,
            )
        };
        let _ = fixture.finish();
        assert_eq!(result, Err(Cause::Deadline));
        assert_eq!(primary, Some(Cause::Deadline));
        assert_eq!(count, 3);
        assert_eq!(digest, None);
        assert!(eof);
    }
    #[test]
    fn exceptional_fixture_setup_still_finishes_every_admitted_original() {
        let _ = close_events();
        let refused = std::panic::catch_unwind(|| {
            let mut fixture = Fixture::new();
            let (left, right) = pair();
            let _left = fixture.push(left);
            let right = fixture.push(right);
            fixture.prepare().unwrap();
            let (owner, slots, _) = fixture.parts();
            rustix::net::sockopt::set_socket_passcred(fd(slots, right), true).unwrap();
            let _channel = process::Channel::new(fd(slots, right).as_fd(), false, owner)
                .expect("fixed setup refused");
        });
        let closed = close_events();
        assert!(refused.is_err());
        assert_eq!(closed, vec![true, true]);
    }
    #[test]
    fn readonly_client_profile_rejects_mutating_or_extra_kernel_argv() {
        let correct = concat!(
            "timeout\0--signal=TERM\0--kill-after=5s\0180s\0",
            "/usr/local/bin/simplestchat-public\0ps\0--status\0",
            "running\0--quiet\0simplestchat\0",
        )
        .as_bytes();
        let mutating = concat!(
            "timeout\0--signal=TERM\0--kill-after=5s\0180s\0",
            "/usr/local/bin/simplestchat-public\0up\0--status\0",
            "running\0--quiet\0simplestchat\0",
        )
        .as_bytes();
        assert!(identity::readonly_client(correct));
        for wrong in [mutating.to_vec(), [correct, b"extra\0"].concat()] {
            assert!(!identity::readonly_client(&wrong));
        }
    }
    #[test]
    fn kernel_pid_record_parser_rejects_dead_duplicate_and_noncanonical_rows() {
        for value in ["0", "-1", "01", "1 0", "4294967296"] {
            assert!(identity::numbers(value).is_err());
        }
        assert!(identity::fields(b"Pid:\t1\nPid:\t2\nNSpid:\t1\n").is_err());
        assert_eq!(identity::numbers("42 1").unwrap(), vec![42, 1]);
    }
}
