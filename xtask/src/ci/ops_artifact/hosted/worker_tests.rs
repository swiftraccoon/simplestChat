//! Same held File, cooperative cancellation and checked close before return.

use super::{failure, fixture, plan, worker};
use crate::{
    ci::{command::Command, process::Executor},
    tooling::Result,
};
use std::{
    fs::File,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::oneshot;

const WATCHDOG: Duration = Duration::from_secs(10);
struct Pending {
    code: Arc<AtomicU8>,
    calls: Arc<AtomicUsize>,
    ready_again: Option<oneshot::Sender<()>>,
    late: bool,
}
impl Executor for Pending {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("no worker subprocess")
    }
    async fn ready(&mut self) -> Result<()> {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst);
        if calls > 0
            && let Some(ready) = self.ready_again.take()
        {
            let _ = ready.send(());
        }
        let code = if self.late && calls > 0 {
            143
        } else {
            self.code.load(Ordering::SeqCst)
        };
        if code == 0 {
            Ok(())
        } else {
            Err(failure(code))
        }
    }
}
struct Release(Option<mpsc::Sender<()>>);
impl Release {
    fn send(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.send();
    }
}

#[tokio::test]
async fn cancellation_and_deadline_keep_parent_responsive_until_same_worker_file_closes() {
    for code in [130, 143, 124] {
        let fixture = fixture::Fixture::new();
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let end = plan.now().unwrap() + Duration::from_secs(60);
        let cancelled = Arc::new(AtomicU8::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut executor = Pending {
            code: cancelled.clone(),
            calls: calls.clone(),
            ready_again: Some(ready_tx),
            late: false,
        };
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let joined = Arc::new(AtomicBool::new(false));
        let worker_joined = joined.clone();
        let path = fixture.root.join("one-held-file");
        let worker_path = path.clone();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (noticed_tx, noticed_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = Release(Some(release_tx));
        let mut operation = Box::pin(worker::work(
            &plan,
            &mut executor,
            end,
            move |checkpoint| {
                let mut file = File::create(worker_path).map_err(|_| failure(1))?;
                let written = file.write_all(b"same owned file\n").map_err(|_| failure(1));
                let result = if written.is_ok() {
                    let _ = entered_tx.send(());
                    let error = loop {
                        if let Err(error) = checkpoint.check() {
                            break error;
                        }
                        match release_rx.recv_timeout(Duration::from_millis(1)) {
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            _ => break failure(1),
                        }
                    };
                    let _ = noticed_tx.send(error.code);
                    let _ = release_rx.recv_timeout(WATCHDOG);
                    Err(error)
                } else {
                    written
                };
                let settled = nix::unistd::close(file).map_err(|_| failure(1));
                worker_closed.store(settled.is_ok(), Ordering::SeqCst);
                worker_joined.store(true, Ordering::SeqCst);
                result.and(settled)
            },
            |()| Ok(()),
        ));
        tokio::select! {biased;
            value=&mut operation=>panic!("worker returned before fixture entry: {value:?}"),
            value=tokio::time::timeout(WATCHDOG,entered_rx)=>assert!(matches!(value,Ok(Ok(())))),
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"same owned file\n");
        if code == 124 {
            clock.advance(Duration::from_secs(61));
        } else {
            cancelled.store(code, Ordering::SeqCst);
        }
        tokio::select! {biased;
            value=&mut operation=>panic!("worker escaped owned File: {value:?}"),
            value=tokio::time::timeout(WATCHDOG,noticed_rx)=>assert!(matches!(value,Ok(Ok(124)))),
        }
        // A worker deadline checkpoint can win before the parent's next tick.
        // Keep polling this same operation until the parent actually rechecks
        // readiness while the worker still owns the File and awaits release.
        tokio::select! {biased;
            value=&mut operation=>panic!("worker escaped parent readiness check: {value:?}"),
            value=tokio::time::timeout(WATCHDOG,ready_rx)=>assert!(matches!(value,Ok(Ok(())))),
        }
        assert!(!closed.load(Ordering::SeqCst));
        assert!(!joined.load(Ordering::SeqCst));
        assert!(calls.load(Ordering::SeqCst) > 1);
        release.send();
        let result = tokio::time::timeout(WATCHDOG, &mut operation)
            .await
            .unwrap();
        fixture::expect_error(result, code);
        assert!(closed.load(Ordering::SeqCst));
        assert!(joined.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn worker_failure_is_retained_and_positive_value_settles_on_late_cancel() {
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock);
    let end = plan.now().unwrap() + Duration::from_secs(60);
    let mut executor = Pending {
        code: Arc::new(AtomicU8::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
        ready_again: None,
        late: false,
    };
    let result = worker::work(
        &plan,
        &mut executor,
        end,
        |_| Err::<(), _>(failure(42)),
        |()| Ok(()),
    )
    .await;
    fixture::expect_error(result, 42);

    for code in [130, 143, 124] {
        let fixture = fixture::Fixture::new();
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let end = plan.now().unwrap() + Duration::from_secs(60);
        let cancelled = Arc::new(AtomicU8::new(0));
        let worker_cancelled = cancelled.clone();
        let mut executor = Pending {
            code: cancelled,
            calls: Arc::new(AtomicUsize::new(0)),
            ready_again: None,
            late: false,
        };
        let returned = Arc::new(AtomicBool::new(false));
        let worker_returned = returned.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let settled = closed.clone();
        let path = fixture.root.join("late-positive-file");
        let worker_path = path.clone();
        let result = worker::work(
            &plan,
            &mut executor,
            end,
            move |_| {
                let mut file = File::create(worker_path).map_err(|_| failure(1))?;
                if file.write_all(b"late positive owned bytes\n").is_err() {
                    let _ = nix::unistd::close(file);
                    return Err(failure(1));
                }
                if code == 124 {
                    clock.advance(Duration::from_secs(61));
                } else {
                    worker_cancelled.store(code, Ordering::SeqCst);
                }
                // Deliberately ignore abort: the owner must settle even a
                // successful returned File whose acceptance is already late.
                worker_returned.store(true, Ordering::SeqCst);
                Ok(file)
            },
            move |file| {
                use std::os::fd::AsRawFd;
                let descriptor = format!("/dev/fd/{}", file.as_raw_fd());
                let original = std::fs::metadata(&descriptor).map_err(|_| failure(1));
                let close = nix::unistd::close(file).map_err(|_| failure(1));
                let original = original?;
                close?;
                let gone = !std::fs::metadata(descriptor)
                    .is_ok_and(|stamp| super::super::namespace::binding(&original, &stamp));
                settled.store(gone, Ordering::SeqCst);
                if gone { Ok(()) } else { Err(failure(1)) }
            },
        )
        .await;
        fixture::expect_error(result, code);
        assert!(returned.load(Ordering::SeqCst));
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(std::fs::read(path).unwrap(), b"late positive owned bytes\n");
    }
}
