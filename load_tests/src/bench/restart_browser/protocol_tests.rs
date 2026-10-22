use super::*;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const RUN: &str = "00000000-0000-4000-8000-000000000001";
struct ClockOwner(Arc<AtomicU64>);
#[async_trait::async_trait]
impl Clock for ClockOwner {
    fn elapsed_ms(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        String::new()
    }
    async fn sleep(&self, _: Duration) {
        unreachable!()
    }
}
struct Channel {
    chunks: VecDeque<Vec<u8>>,
    clock: Arc<AtomicU64>,
    advance: u64,
    closed: Arc<AtomicBool>,
    writes: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}
#[async_trait::async_trait]
impl Control for Channel {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let bytes = self.chunks.pop_front().unwrap_or_default();
        self.clock.fetch_add(self.advance, Ordering::SeqCst);
        ensure!(bytes.len() <= buffer.len(), "inert_control_oversized");
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
    async fn write(&mut self, _: &[u8]) -> Result<()> {
        unreachable!()
    }
    async fn write_bounded(&mut self, bytes: &[u8], limit: Duration) -> Result<()> {
        assert_eq!(limit, Duration::from_secs(2));
        self.writes.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}
fn frame(sequence: u8, phase: &str) -> Vec<u8> {
    format!(
        concat!(
            "{{\"schemaVersion\":1,\"runId\":\"{}\",",
            "\"sequence\":{},\"phase\":\"{}\"}}\n"
        ),
        RUN, sequence, phase
    )
    .into_bytes()
}
fn session(chunks: Vec<Vec<u8>>, advance: u64) -> (Session, ClockOwner, Arc<AtomicBool>) {
    let clock = Arc::new(AtomicU64::new(0));
    let closed = Arc::new(AtomicBool::new(false));
    let channel = Channel {
        chunks: chunks.into(),
        clock: clock.clone(),
        advance,
        closed: closed.clone(),
        writes: Default::default(),
    };
    (Session::new(Box::new(channel)), ClockOwner(clock), closed)
}
#[tokio::test]
async fn ordered_canonical_requests_require_each_ack_and_final_eof() {
    let (mut session, clock, closed) = session(
        PHASES
            .iter()
            .enumerate()
            .map(|(i, p)| frame(i as u8 + 1, p))
            .collect(),
        0,
    );
    for (index, phase) in PHASES.iter().enumerate() {
        assert_eq!(
            session.poll(RUN, &clock).await.unwrap().unwrap().phase,
            *phase
        );
        assert_eq!(session.sequence, index as u8 + 1);
        assert!(!session.complete());
        session.reply(RUN, 0).await.unwrap();
    }
    assert!(session.complete());
    assert!(session.poll(RUN, &clock).await.unwrap().is_none());
    session.settle(&clock).await.unwrap();
    session.finish().unwrap();
    assert!(closed.load(Ordering::SeqCst));
}
#[tokio::test]
async fn completed_fragment_after_two_seconds_is_refused_before_acceptance() {
    let bytes = frame(1, "stop-first");
    let cut = bytes.len() / 2;
    let (mut session, clock, closed) =
        session(vec![bytes[..cut].to_vec(), bytes[cut..].to_vec()], 2001);
    assert!(session.poll(RUN, &clock).await.unwrap().is_none());
    let error = session.poll(RUN, &clock).await.err().unwrap();
    assert_eq!(
        crate::bench::restart_browser::controller::status(&error),
        124
    );
    assert_eq!(session.sequence, 0);
    session.finish().unwrap();
    assert!(closed.load(Ordering::SeqCst));
}
#[tokio::test]
async fn malformed_foreign_duplicate_and_noncanonical_frames_refuse_phase_admission() {
    let valid = String::from_utf8(frame(1, "stop-first")).unwrap();
    let mut unknown: Value = serde_json::from_str(&valid).unwrap();
    unknown["private"] = true.into();
    let mut unknown = serde_json::to_vec(&unknown).unwrap();
    unknown.push(b'\n');
    for bytes in [
        valid.replace(RUN, "foreign").into_bytes(),
        frame(2, "start-second"),
        valid.replace("{", "{ ").into_bytes(),
        format!("{valid}{valid}").into_bytes(),
        valid
            .replace(
                "\"schemaVersion\":1",
                "\"schemaVersion\":1,\"schemaVersion\":1",
            )
            .into_bytes(),
        unknown,
        vec![b'x'; 1025],
    ] {
        let (mut session, clock, closed) = session(vec![bytes], 0);
        assert!(session.poll(RUN, &clock).await.is_err());
        assert_eq!(session.sequence, 0);
        assert!(session.settle(&clock).await.is_err());
        session.finish().unwrap();
        assert!(closed.load(Ordering::SeqCst));
    }
}
#[tokio::test]
async fn incomplete_or_trailing_input_still_allows_explicit_endpoint_close() {
    for complete in [false, true] {
        let (mut session, clock, closed) = session(vec![vec![b'x']], 0);
        if complete {
            session.sequence = 3;
            session.acknowledged = true;
        }
        assert!(session.settle(&clock).await.is_err());
        session.finish().unwrap();
        assert!(closed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn eof_first_observed_at_or_after_two_seconds_is_refused_and_the_endpoint_closes() {
    for after in [2000, 2001] {
        let (mut session, clock, closed) = session(vec![], after);
        session.sequence = 3;
        session.acknowledged = true;
        let error = session.settle(&clock).await.err().unwrap();
        assert_eq!(
            crate::bench::restart_browser::controller::status(&error),
            124
        );
        assert!(!closed.load(Ordering::SeqCst));
        session.finish().unwrap();
        assert!(closed.load(Ordering::SeqCst));
    }
}
