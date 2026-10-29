//! Inert Linux FD inheritance and the shared guest spawn window; no root effects.
#![cfg(target_os = "linux")]

use super::*;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        TryLockError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const CHILD: &str = "SIMPLESTCHAT_DEPLOY_FD_TEST";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "deploy-fd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let root = Self(fs::canonicalize(path).unwrap());
        fs::write(root.0.join("original"), b"owned inert inherited input\n").unwrap();
        fs::set_permissions(root.0.join("original"), fs::Permissions::from_mode(0o600)).unwrap();
        root
    }
    fn original(&self, control: &mut Control) -> Original {
        Original::fixture(&self.0.join("original"), 128, 0o600, control).unwrap()
    }
    fn close(&self) -> std::io::Result<()> {
        fs::remove_file(self.0.join("original"))?;
        fs::remove_dir(&self.0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("original"));
        let _ = fs::remove_dir(&self.0);
    }
}

fn child_command(original: &Original, duplicate: i32, inherited: bool) -> Command {
    let name = concat!(module_path!(), "::inert_deploy_fd_child");
    let name = name.split_once("::").unwrap().1;
    let metadata = original.file().unwrap().metadata().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--test-threads=1"])
        .env_clear()
        .env(CHILD, if inherited { "inherited" } else { "closed" })
        .env(
            "SC_FD_ORIGINAL",
            original.file().unwrap().as_raw_fd().to_string(),
        )
        .env("SC_FD_DUPLICATE", duplicate.to_string())
        .env("SC_FD_DEV", metadata.dev().to_string())
        .env("SC_FD_INO", metadata.ino().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

#[test]
fn inert_deploy_fd_child() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    let number = |key: &str| std::env::var(key).unwrap().parse::<u64>().unwrap();
    let dev = number("SC_FD_DEV");
    let ino = number("SC_FD_INO");
    let refers_to_original = |key: &str| {
        fs::metadata(format!("/proc/self/fd/{}", number(key)))
            .is_ok_and(|metadata| metadata.dev() == dev && metadata.ino() == ino)
    };
    assert!(!refers_to_original("SC_FD_ORIGINAL"));
    assert_eq!(refers_to_original("SC_FD_DUPLICATE"), mode == "inherited");
    assert!(matches!(mode.as_str(), "inherited" | "closed"));
}

async fn settle(mut child: Group, end: Instant) -> (bool, bool, bool, Option<i32>) {
    let pid = child.pid();
    let natural = tokio::time::timeout_at(
        tokio::time::Instant::from_std(end - Duration::from_secs(3)),
        crate::owned_process::exited(pid),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let retired = finish_before(pid, end).await;
    let status = child.reap().await;
    (
        natural,
        retired,
        status.is_ok() && !reserved(pid),
        status.ok().and_then(|s| s.code()),
    )
}

fn closed_duplicate(number: i32, original: &Original) -> bool {
    let expected = original.file().unwrap().metadata().unwrap();
    // A parallel test may reuse the number; it must not retain our unique inode.
    !fs::metadata(format!("/proc/self/fd/{number}"))
        .is_ok_and(|now| now.dev() == expected.dev() && now.ino() == expected.ino())
}

#[derive(Clone, Copy)]
enum Case {
    Success,
    SpawnFailure,
    RestoreFailure,
}

async fn handoff_case(case: Case) {
    let fixture = Fixture::new();
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    control.push_phase(Duration::from_secs(30)).unwrap();
    let end = control.phase_end();
    let mut original = fixture.original(&mut control);
    let original_flags = rustix::io::fcntl_getfd(original.file().unwrap()).unwrap();
    let mut duplicate = Inherited::new(&original, &mut control).unwrap();
    let number = duplicate.number().unwrap();
    let mut command = child_command(&original, number, true);
    if matches!(case, Case::SpawnFailure) {
        command = Command::new(fixture.0.join("absent-executable"));
    }
    let (enabled, flags_unchanged, spawned, restored_flags, finished, no_duplicate, still_locked) = {
        let _gate = SPAWN_GATE.lock().unwrap();
        let enabled = duplicate.enable();
        let flags_unchanged = rustix::io::fcntl_getfd(original.file().unwrap());
        let spawned = if enabled.is_ok() {
            command.spawn().map(Group::new).map_err(|_| Error::Io)
        } else {
            Err(Error::Io)
        };
        if let Err(error) = &spawned {
            control.record(*error);
        }
        let mut restored_flags = None;
        let finished = duplicate.finish_with(&mut control, |file, flags| {
            rustix::io::fcntl_setfd(file, flags).map_err(|_| Error::Close)?;
            restored_flags = rustix::io::fcntl_getfd(file).ok();
            if matches!(case, Case::RestoreFailure) {
                Err(Error::Close)
            } else {
                Ok(())
            }
        });
        let no_duplicate = duplicate.file.is_none() && closed_duplicate(number, &original);
        let still_locked = matches!(SPAWN_GATE.try_lock(), Err(TryLockError::WouldBlock));
        (
            enabled,
            flags_unchanged,
            spawned,
            restored_flags,
            finished,
            no_duplicate,
            still_locked,
        )
    };
    let failed_spawn = spawned.is_err();
    let child = if let Ok(child) = spawned {
        Some(settle(child, end).await)
    } else {
        None
    };
    let cleanup_failed = control.cleanup_failed;
    let first = control.first;
    control.begin_settling();
    let closed = original.finish(&mut control);
    let owner_finished = control.finish();
    let removed = fixture.close();
    assert!(original_flags.contains(rustix::io::FdFlags::CLOEXEC));
    assert!(enabled.is_ok());
    assert_eq!(flags_unchanged.ok(), Some(original_flags));
    assert_eq!(restored_flags, Some(original_flags));
    assert!(no_duplicate && still_locked);
    assert_eq!(failed_spawn, matches!(case, Case::SpawnFailure));
    if let Some(facts) = child {
        assert_eq!(facts, (true, true, true, Some(0)));
    }
    assert_eq!(finished.is_err(), matches!(case, Case::RestoreFailure));
    assert_eq!(cleanup_failed, matches!(case, Case::RestoreFailure));
    let expected = match case {
        Case::Success => None,
        Case::SpawnFailure => Some(Error::Io),
        Case::RestoreFailure => Some(Error::Close),
    };
    assert_eq!(first, expected);
    assert!(closed.is_ok());
    assert_eq!(owner_finished.err(), expected);
    assert!(removed.is_ok());
    assert!(Instant::now() < end);
}

#[tokio::test(flavor = "current_thread")]
async fn deploy_duplicate_alone_crosses_exec_and_closes_before_gate_release() {
    handoff_case(Case::Success).await;
}
#[tokio::test(flavor = "current_thread")]
async fn deploy_spawn_failure_restores_and_closes_before_gate_release() {
    handoff_case(Case::SpawnFailure).await;
}
#[tokio::test(flavor = "current_thread")]
async fn deploy_restore_observer_failure_closes_and_preserves_cleanup_failure() {
    handoff_case(Case::RestoreFailure).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_concurrent_spawn_waits_until_owned_duplicate_is_closed() {
    let fixture = Fixture::new();
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    control.push_phase(Duration::from_secs(30)).unwrap();
    let end = control.phase_end();
    let mut original = fixture.original(&mut control);
    let mut duplicate = Inherited::new(&original, &mut control).unwrap();
    let number = duplicate.number().unwrap();
    let mut command = child_command(&original, number, false);
    let (sender, receiver) = mpsc::channel();
    let (enabled, worker, blocked, finished, no_duplicate, unchanged) = {
        let _gate = SPAWN_GATE.lock().unwrap();
        let enabled = duplicate.enable();
        let worker = std::thread::spawn(move || {
            let blocked = matches!(SPAWN_GATE.try_lock(), Err(TryLockError::WouldBlock));
            let _ = sender.send(blocked);
            loop {
                match SPAWN_GATE.try_lock() {
                    Ok(_gate) => return command.spawn().map(Group::new),
                    Err(TryLockError::WouldBlock) if Instant::now() < end => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    _ => return Err(std::io::Error::other("fixed test spawn gate refused")),
                }
            }
        });
        let blocked = receiver.recv_timeout(Duration::from_secs(3));
        let finished = duplicate.finish(&mut control);
        let no_duplicate = duplicate.file.is_none() && closed_duplicate(number, &original);
        let unchanged = rustix::io::fcntl_getfd(original.file().unwrap())
            .is_ok_and(|flags| flags.contains(rustix::io::FdFlags::CLOEXEC));
        (enabled, worker, blocked, finished, no_duplicate, unchanged)
    };
    let spawned = worker.join();
    let facts = match spawned {
        Ok(Ok(child)) => Some(settle(child, end).await),
        _ => None,
    };
    control.begin_settling();
    let closed = original.finish(&mut control);
    let owner_finished = control.finish();
    let removed = fixture.close();
    assert!(enabled.is_ok());
    assert_eq!(blocked.ok(), Some(true));
    assert!(finished.is_ok() && no_duplicate && unchanged);
    assert_eq!(facts, Some((true, true, true, Some(0))));
    assert!(closed.is_ok() && owner_finished.is_ok() && removed.is_ok());
    assert!(Instant::now() < end);
}
