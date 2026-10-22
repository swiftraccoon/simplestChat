//! Actual blocking worker ownership with tiny files and deterministic gates.
//! Watchdogs bound broken fixtures; no elapsed-performance assertion is used.

use super::{failure, fixture, hash, plan};
use crate::{
    ci::{command::Command, process::Executor},
    tooling::Result,
};
use std::{
    fs::{self, File},
    future::{Future, poll_fn},
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

const WATCHDOG: Duration = Duration::from_secs(10);

struct Pending {
    cancelled: Arc<AtomicU8>,
    ready_calls: Arc<AtomicUsize>,
    just_ready: Option<Arc<AtomicBool>>,
}
impl Executor for Pending {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("filesystem worker fixtures must never dispatch a CLI")
    }
    async fn ready(&mut self) -> Result<()> {
        self.ready_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(flag) = &self.just_ready {
            flag.store(true, Ordering::SeqCst);
        }
        match self.cancelled.load(Ordering::SeqCst) {
            0 => Ok(()),
            code => Err(failure(code)),
        }
    }
}
struct Release(Option<mpsc::Sender<()>>);
impl Release {
    fn send(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.send();
    }
}

#[tokio::test]
async fn observed_signal_or_deadline_returns_only_after_same_held_file_closes_and_worker_joins() {
    for code in [130, 143, 124] {
        let fixture = fixture::Fixture::new(0);
        let path = fixture.root.join("held-worker-file");
        let clock = Arc::new(fixture::Clock::new());
        let context = plan::Context::new(clock.clone());
        let cancelled = Arc::new(AtomicU8::new(0));
        let ready_calls = Arc::new(AtomicUsize::new(0));
        let mut executor = Pending {
            cancelled: cancelled.clone(),
            ready_calls: ready_calls.clone(),
            just_ready: None,
        };
        let closed = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicUsize::new(0));
        let worker_closed = closed.clone();
        let worker_finished = finished.clone();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (noticed_tx, noticed_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = Release(Some(release_tx));
        let worker_path = path.clone();
        let mut operation = Box::pin(hash::work(&context, &mut executor, move |checkpoint| {
            let mut file = File::create(worker_path).map_err(|_| failure(1))?;
            let written = file.write_all(b"one owned file\n").map_err(|_| failure(1));
            let result = if written.is_ok() {
                let _ = entered_tx.send(());
                let (error, was_checkpoint) = loop {
                    if let Err(error) = checkpoint.check() {
                        break (error, true);
                    }
                    match release_rx.recv_timeout(Duration::from_millis(1)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break (failure(1), false),
                    }
                };
                let _ = noticed_tx.send(error.code);
                // Keep exactly this File live after abort was observed. The
                // async owner must continue polling/joining, not return early.
                if was_checkpoint {
                    let _ = release_rx.recv_timeout(WATCHDOG);
                }
                Err(error)
            } else {
                written
            };
            let close = nix::unistd::close(file).map_err(|_| failure(1));
            worker_closed.store(close.is_ok(), Ordering::SeqCst);
            worker_finished.fetch_add(1, Ordering::SeqCst);
            let _ = done_tx.send(());
            result.and(close)
        }));
        let mut early = None;
        let entered = tokio::select! {
            biased;
            value = &mut operation => { early = Some(value); false },
            value = tokio::time::timeout(WATCHDOG, entered_rx) => matches!(value, Ok(Ok(()))),
        };
        let bytes_while_open = fs::read(&path);
        if entered {
            if code == 124 {
                clock.advance(plan::HASH);
            } else {
                cancelled.store(code, Ordering::SeqCst);
            }
        }
        let noticed = if early.is_none() {
            tokio::select! {
                biased;
                value = &mut operation => { early = Some(value); None },
                value = tokio::time::timeout(WATCHDOG, noticed_rx) => {
                    value.ok().and_then(|value| value.ok())
                },
            }
        } else {
            None
        };
        // Poll once without dropping the retained future. With the worker held
        // after its checkpoint, neither a signal nor timeout permits completion.
        if early.is_none() {
            let observed = poll_fn(|cx| match operation.as_mut().poll(cx) {
                Poll::Ready(value) => Poll::Ready(Some(value)),
                Poll::Pending => Poll::Ready(None),
            })
            .await;
            early = observed;
        }
        let returned_before_release = early.is_some();
        let was_open = !closed.load(Ordering::SeqCst);
        let incomplete = finished.load(Ordering::SeqCst) == 0;
        release.send();
        let result = match early {
            Some(value) => value,
            None => operation.await,
        };
        let worker_done = tokio::time::timeout(WATCHDOG, done_rx).await;
        // All assertions follow releasing the gate and awaiting the owner.
        assert!(entered);
        assert_eq!(bytes_while_open.unwrap(), b"one owned file\n");
        assert_eq!(noticed, Some(124));
        assert!(!returned_before_release);
        assert!(was_open && incomplete);
        assert!(matches!(worker_done, Ok(Ok(()))));
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert_eq!(result.unwrap_err().code, code);
        if code != 124 {
            assert!(ready_calls.load(Ordering::SeqCst) >= 2);
        } else {
            assert_eq!(context.hash_used.get(), plan::HASH);
        }
    }
}

/// Inject a pending signal at the post-JOIN clock observation. Every ordinary
/// in-loop clock sample follows ready(), so this trigger does not depend on a
/// fast worker beating the async owner's ten-millisecond polling timer.
struct PostJoinClock {
    origin: Instant,
    just_ready: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    cancelled: Arc<AtomicU8>,
    injected: Arc<AtomicBool>,
}
impl plan::Clock for PostJoinClock {
    fn now(&self) -> Instant {
        let after_ready = self.just_ready.swap(false, Ordering::SeqCst);
        if self.closed.load(Ordering::SeqCst) && !after_ready {
            self.cancelled.store(143, Ordering::SeqCst);
            self.injected.store(true, Ordering::SeqCst);
        }
        self.origin
    }
}

#[tokio::test]
async fn known_joined_worker_failure_precedes_a_signal_at_the_post_join_fence() {
    let fixture = fixture::Fixture::new(0);
    let path = fixture.root.join("failed-worker-file");
    let just_ready = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicU8::new(0));
    let injected = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(PostJoinClock {
        origin: Instant::now(),
        just_ready: just_ready.clone(),
        closed: closed.clone(),
        cancelled: cancelled.clone(),
        injected: injected.clone(),
    });
    let context = plan::Context::new(clock);
    let mut executor = Pending {
        cancelled: cancelled.clone(),
        ready_calls: Arc::new(AtomicUsize::new(0)),
        just_ready: Some(just_ready),
    };
    let worker_closed = closed.clone();
    let result = hash::work(&context, &mut executor, move |_| {
        let mut file = File::create(path).map_err(|_| failure(1))?;
        let written = file
            .write_all(b"failed owned worker\n")
            .map_err(|_| failure(1));
        let close = nix::unistd::close(file).map_err(|_| failure(1));
        worker_closed.store(close.is_ok(), Ordering::SeqCst);
        written.and(Err::<(), _>(failure(42))).and(close)
    })
    .await;
    assert!(closed.load(Ordering::SeqCst));
    assert!(injected.load(Ordering::SeqCst));
    assert_eq!(cancelled.load(Ordering::SeqCst), 143);
    assert_eq!(result.unwrap_err().code, 42);
}
