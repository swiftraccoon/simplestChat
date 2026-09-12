// Bounded, per-attempt subscription scheduling for owned synthetic clients.
// Retired inventory is filtered before sending a new Consume. Retirement is
// not permission to evict an existing native consumer or release its slot.

use anyhow::{Result, anyhow};
use mediasoup::prelude::MediaKind;
use std::collections::{HashMap, VecDeque};

const MAX_RETAINED_PRODUCERS: usize = 20_000;

#[derive(Debug, PartialEq, Eq)]
pub struct ProducerRequest {
    pub producer_id: String,
    pub kind: MediaKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Requested,
    Active,
    /// Never requested: disabled kind or authoritatively retired inventory.
    Ignored,
    /// Actual server closure, including closure before discovery/response.
    Closed,
}

#[derive(Debug)]
struct Producer {
    kind: Option<MediaKind>,
    phase: Phase,
    consumer_id: Option<String>,
}

/// FIFO within each kind; fair inspection between kinds with available slots.
/// All retained IDs, including terminal tombstones, count against one limit.
#[derive(Debug)]
pub struct Subscriptions {
    limits: [usize; 2],
    reserved: [usize; 2],
    pending: [VecDeque<String>; 2],
    producers: HashMap<String, Producer>,
    consumer_owners: HashMap<String, String>,
    next_kind: usize,
    stopped: bool,
}

const fn kind_index(kind: MediaKind) -> usize {
    match kind {
        MediaKind::Audio => 0,
        MediaKind::Video => 1,
    }
}

impl Subscriptions {
    pub fn new(max_audio: usize, max_video: usize) -> Self {
        Self {
            limits: [max_audio, max_video],
            reserved: [0, 0],
            pending: [VecDeque::new(), VecDeque::new()],
            producers: HashMap::new(),
            consumer_owners: HashMap::new(),
            next_kind: 0,
            stopped: false,
        }
    }

    fn fail<T>(&mut self, reason: &str) -> Result<T> {
        self.stop();
        Err(anyhow!("{reason}"))
    }

    fn ensure_room(&mut self) -> Result<()> {
        if self.producers.len() >= MAX_RETAINED_PRODUCERS {
            self.fail("Subscription inventory limit exceeded; dispatch stopped")
        } else {
            Ok(())
        }
    }

    /// Discovery does not reserve capacity. Repeat events cannot reorder FIFO
    /// or revive a terminal producer. Unknown owned lifecycle means retired=false.
    pub fn discover(&mut self, producer_id: String, kind: MediaKind, retired: bool) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        if let Some(existing) = self.producers.get_mut(&producer_id) {
            if existing.kind.is_some_and(|previous| previous != kind) {
                return self.fail("Producer rediscovery changed media kind");
            }
            existing.kind = Some(kind);
            if retired && existing.phase == Phase::Pending {
                existing.phase = Phase::Ignored;
            }
            return Ok(());
        }
        self.ensure_room()?;
        let index = kind_index(kind);
        let phase = if retired || self.limits[index] == 0 {
            Phase::Ignored
        } else {
            Phase::Pending
        };
        self.producers.insert(
            producer_id.clone(),
            Producer {
                kind: Some(kind),
                phase,
                consumer_id: None,
            },
        );
        if phase == Phase::Pending {
            self.pending[index].push_back(producer_id);
        }
        Ok(())
    }

    fn kind_has_work(&self, index: usize) -> bool {
        self.reserved[index] < self.limits[index] && !self.pending[index].is_empty()
    }

    pub fn has_work(&self) -> bool {
        !self.stopped && (self.kind_has_work(0) || self.kind_has_work(1))
    }

    /// Inspect at most one queued ID, including an ignored/closed/retired head.
    /// A None result can therefore mean progress, not an empty queue. The caller
    /// bounds inspections per timer turn; this function never scans or waits.
    pub fn next_request(
        &mut self,
        mut retired: impl FnMut(&str) -> bool,
    ) -> Option<ProducerRequest> {
        if self.stopped {
            return None;
        }
        let index = if self.kind_has_work(self.next_kind) {
            self.next_kind
        } else if self.kind_has_work(1 - self.next_kind) {
            1 - self.next_kind
        } else {
            return None;
        };
        self.next_kind = 1 - index;
        let producer_id = self.pending[index].pop_front()?;
        let producer = self.producers.get_mut(&producer_id)?;
        if producer.phase != Phase::Pending {
            return None;
        }
        if retired(&producer_id) {
            producer.phase = Phase::Ignored;
            return None;
        }
        let kind = producer.kind?;
        producer.phase = Phase::Requested;
        self.reserved[index] += 1;
        Some(ProducerRequest { producer_id, kind })
    }

    /// Accept one response for a reserved request. Closure races and identical
    /// duplicates are harmless; neither reopens a slot or creates another peer.
    pub fn created(
        &mut self,
        producer_id: &str,
        kind: MediaKind,
        consumer_id: &str,
    ) -> Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        let Some(producer) = self.producers.get(producer_id) else {
            return self.fail("ConsumerCreated references an unknown producer");
        };
        if producer.kind.is_some_and(|expected| expected != kind) {
            return self.fail("ConsumerCreated changed media kind");
        }
        if self
            .consumer_owners
            .get(consumer_id)
            .is_some_and(|owner| owner != producer_id)
        {
            return self.fail("Consumer ID is already assigned to another producer");
        }
        if let Some(previous) = &producer.consumer_id {
            if previous != consumer_id {
                return self.fail("Producer returned conflicting consumer IDs");
            }
            return Ok(false);
        }
        let accepted = match producer.phase {
            Phase::Requested => true,
            Phase::Closed => false,
            Phase::Pending | Phase::Ignored | Phase::Active => {
                return self.fail("ConsumerCreated was not requested");
            }
        };
        let producer = self
            .producers
            .get_mut(producer_id)
            .expect("Producer was checked above");
        producer.kind = Some(kind);
        producer.consumer_id = Some(consumer_id.to_string());
        if accepted {
            producer.phase = Phase::Active;
        }
        self.consumer_owners
            .insert(consumer_id.to_string(), producer_id.to_string());
        Ok(accepted)
    }

    /// Called only for actual server ProducerClosed. Pending entries are lazily
    /// discarded at their FIFO position. Metrics separately decide whether this
    /// server closure was unexpected; this method never excuses delivery gaps.
    pub fn close(&mut self, producer_id: &str) -> Result<()> {
        if let Some(producer) = self.producers.get_mut(producer_id) {
            if matches!(producer.phase, Phase::Requested | Phase::Active) {
                let index = kind_index(producer.kind.expect("Reserved producer has a kind"));
                let Some(remaining) = self.reserved[index].checked_sub(1) else {
                    return self.fail("Subscription reservation accounting underflow");
                };
                self.reserved[index] = remaining;
            }
            producer.phase = Phase::Closed;
            return Ok(());
        }
        if self.stopped {
            return Ok(());
        }
        self.ensure_room()?;
        self.producers.insert(
            producer_id.to_string(),
            Producer {
                kind: None,
                phase: Phase::Closed,
                consumer_id: None,
            },
        );
        Ok(())
    }

    pub fn can_resume(&self, consumer_id: &str) -> bool {
        !self.stopped
            && self
                .consumer_owners
                .get(consumer_id)
                .and_then(|producer| self.producers.get(producer))
                .is_some_and(|producer| producer.phase == Phase::Active)
    }

    /// A failed write may still have reached the server. Stop dispatch without
    /// freeing reservations or retrying; actual closes may still release slots.
    pub fn stop(&mut self) {
        self.stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discover(subscriptions: &mut Subscriptions, id: &str, kind: MediaKind) {
        subscriptions.discover(id.to_string(), kind, false).unwrap();
    }

    fn request(subscriptions: &mut Subscriptions) -> ProducerRequest {
        subscriptions
            .next_request(|_| false)
            .expect("Expected a queued request")
    }

    #[test]
    fn fifo_is_per_kind_and_duplicate_discovery_does_not_reorder() {
        let mut subscriptions = Subscriptions::new(2, 2);
        for (id, kind) in [
            ("a1", MediaKind::Audio),
            ("a2", MediaKind::Audio),
            ("v1", MediaKind::Video),
            ("v2", MediaKind::Video),
        ] {
            discover(&mut subscriptions, id, kind);
        }
        discover(&mut subscriptions, "a1", MediaKind::Audio);
        for expected in ["a1", "v1", "a2", "v2"] {
            assert_eq!(request(&mut subscriptions).producer_id, expected);
        }
        discover(&mut subscriptions, "a1", MediaKind::Audio);
        assert!(!subscriptions.has_work());
        assert_eq!(subscriptions.reserved, [2, 2]);
        assert!(subscriptions.pending.iter().all(VecDeque::is_empty));
    }

    #[test]
    fn full_audio_capacity_does_not_block_video_or_drop_audio_backlog() {
        let mut subscriptions = Subscriptions::new(1, 1);
        discover(&mut subscriptions, "a1", MediaKind::Audio);
        assert_eq!(request(&mut subscriptions).producer_id, "a1");
        discover(&mut subscriptions, "a2", MediaKind::Audio);
        discover(&mut subscriptions, "v1", MediaKind::Video);
        assert_eq!(request(&mut subscriptions).producer_id, "v1");
        assert!(!subscriptions.has_work());
        subscriptions.close("a1").unwrap();
        subscriptions.close("a1").unwrap();
        assert!(subscriptions.has_work());
        assert_eq!(request(&mut subscriptions).producer_id, "a2");
        assert_eq!(subscriptions.reserved, [1, 1]);
    }

    #[test]
    fn each_call_inspects_one_head_and_stale_audio_does_not_starve_video() {
        let mut subscriptions = Subscriptions::new(1, 1);
        for id in ["stale-a1", "stale-a2", "a3"] {
            discover(&mut subscriptions, id, MediaKind::Audio);
        }
        discover(&mut subscriptions, "v1", MediaKind::Video);
        let mut inspected = Vec::new();
        assert!(
            subscriptions
                .next_request(|id| {
                    inspected.push(id.to_string());
                    true
                })
                .is_none()
        );
        assert_eq!(inspected, vec!["stale-a1"]);
        assert_eq!(subscriptions.pending[0].len(), 2);
        assert_eq!(request(&mut subscriptions).producer_id, "v1");
        assert!(subscriptions.next_request(|id| id == "stale-a2").is_none());
        assert_eq!(request(&mut subscriptions).producer_id, "a3");
    }

    #[test]
    fn retired_inventory_and_zero_caps_never_reserve_capacity() {
        let mut subscriptions = Subscriptions::new(1, 0);
        subscriptions
            .discover("retired".into(), MediaKind::Audio, true)
            .unwrap();
        discover(&mut subscriptions, "disabled", MediaKind::Video);
        assert!(!subscriptions.has_work());
        assert!(subscriptions.pending.iter().all(VecDeque::is_empty));
        assert_eq!(subscriptions.reserved, [0, 0]);
        discover(&mut subscriptions, "retired", MediaKind::Audio);
        assert!(!subscriptions.has_work());
        discover(
            &mut subscriptions,
            "unknown-owned-lifecycle",
            MediaKind::Audio,
        );
        assert_eq!(
            request(&mut subscriptions).producer_id,
            "unknown-owned-lifecycle"
        );
    }

    #[test]
    fn retirement_before_dispatch_is_rechecked_but_after_dispatch_does_not_evict() {
        let mut subscriptions = Subscriptions::new(1, 0);
        discover(&mut subscriptions, "retiring", MediaKind::Audio);
        assert!(subscriptions.next_request(|_| true).is_none());
        assert_eq!(subscriptions.reserved, [0, 0]);
        discover(&mut subscriptions, "active", MediaKind::Audio);
        assert_eq!(request(&mut subscriptions).producer_id, "active");
        assert!(
            subscriptions
                .created("active", MediaKind::Audio, "consumer")
                .unwrap()
        );
        subscriptions
            .discover("active".into(), MediaKind::Audio, true)
            .unwrap();
        discover(&mut subscriptions, "replacement", MediaKind::Audio);
        assert_eq!(subscriptions.reserved, [1, 0]);
        assert!(!subscriptions.has_work());
        assert!(subscriptions.can_resume("consumer"));
        subscriptions.close("active").unwrap();
        assert!(!subscriptions.can_resume("consumer"));
        assert_eq!(request(&mut subscriptions).producer_id, "replacement");
    }

    #[test]
    fn queued_and_unknown_closes_suppress_stale_discovery_without_reopening() {
        let mut subscriptions = Subscriptions::new(1, 1);
        discover(&mut subscriptions, "queued", MediaKind::Audio);
        subscriptions.close("queued").unwrap();
        subscriptions.close("unknown").unwrap();
        discover(&mut subscriptions, "queued", MediaKind::Audio);
        discover(&mut subscriptions, "unknown", MediaKind::Video);
        assert!(
            subscriptions
                .next_request(|_| panic!("Closed entry must not query retirement"))
                .is_none()
        );
        assert!(!subscriptions.has_work());
        assert_eq!(subscriptions.reserved, [0, 0]);
        assert!(
            !subscriptions
                .created("unknown", MediaKind::Video, "late")
                .unwrap()
        );
        assert!(!subscriptions.can_resume("late"));
    }

    #[test]
    fn identical_created_is_idempotent_and_close_before_response_keeps_slot_free() {
        let mut subscriptions = Subscriptions::new(1, 1);
        discover(&mut subscriptions, "audio", MediaKind::Audio);
        request(&mut subscriptions);
        assert!(
            subscriptions
                .created("audio", MediaKind::Audio, "a-consumer")
                .unwrap()
        );
        assert!(
            !subscriptions
                .created("audio", MediaKind::Audio, "a-consumer")
                .unwrap()
        );
        assert!(subscriptions.can_resume("a-consumer"));
        subscriptions.close("audio").unwrap();
        assert!(
            !subscriptions
                .created("audio", MediaKind::Audio, "a-consumer")
                .unwrap()
        );
        discover(&mut subscriptions, "video", MediaKind::Video);
        request(&mut subscriptions);
        subscriptions.close("video").unwrap();
        assert!(
            !subscriptions
                .created("video", MediaKind::Video, "v-consumer")
                .unwrap()
        );
        assert!(!subscriptions.can_resume("a-consumer"));
        assert!(!subscriptions.can_resume("v-consumer"));
        assert_eq!(subscriptions.reserved, [0, 0]);
    }

    #[test]
    fn conflicting_responses_fail_closed_without_releasing_reservations() {
        for invalid in [
            "wrong-kind",
            "different-consumer",
            "shared-consumer",
            "unknown",
            "not-requested",
        ] {
            let mut subscriptions = Subscriptions::new(2, 0);
            discover(&mut subscriptions, "a1", MediaKind::Audio);
            discover(&mut subscriptions, "a2", MediaKind::Audio);
            discover(&mut subscriptions, "a3", MediaKind::Audio);
            request(&mut subscriptions);
            request(&mut subscriptions);
            assert!(
                subscriptions
                    .created("a1", MediaKind::Audio, "first")
                    .unwrap()
            );
            let result = match invalid {
                "wrong-kind" => subscriptions.created("a2", MediaKind::Video, "second"),
                "different-consumer" => subscriptions.created("a1", MediaKind::Audio, "second"),
                "shared-consumer" => subscriptions.created("a2", MediaKind::Audio, "first"),
                "unknown" => subscriptions.created("unknown", MediaKind::Audio, "second"),
                "not-requested" => subscriptions.created("a3", MediaKind::Audio, "second"),
                _ => unreachable!(),
            };
            assert!(result.is_err(), "{invalid}");
            assert!(!subscriptions.has_work(), "{invalid}");
            assert_eq!(subscriptions.reserved, [2, 0], "{invalid}");
            assert!(!subscriptions.can_resume("first"), "{invalid}");
        }
    }

    #[test]
    fn rediscovery_kind_mismatch_is_an_error_even_for_terminal_ids() {
        let mut subscriptions = Subscriptions::new(1, 1);
        discover(&mut subscriptions, "producer", MediaKind::Audio);
        subscriptions.close("producer").unwrap();
        assert!(
            subscriptions
                .discover("producer".into(), MediaKind::Video, false)
                .is_err()
        );
        assert!(!subscriptions.has_work());
    }

    #[test]
    fn stop_on_ambiguous_write_preserves_slots_until_actual_close() {
        let mut subscriptions = Subscriptions::new(1, 0);
        discover(&mut subscriptions, "in-flight", MediaKind::Audio);
        request(&mut subscriptions);
        subscriptions.stop();
        discover(&mut subscriptions, "new", MediaKind::Audio);
        assert!(!subscriptions.has_work());
        assert!(subscriptions.next_request(|_| false).is_none());
        assert_eq!(subscriptions.reserved, [1, 0]);
        assert!(
            !subscriptions
                .created("in-flight", MediaKind::Audio, "late")
                .unwrap()
        );
        subscriptions.close("in-flight").unwrap();
        assert_eq!(subscriptions.reserved, [0, 0]);
    }

    #[test]
    fn producer_and_tombstone_bound_fails_once_without_eviction() {
        let mut subscriptions = Subscriptions::new(1, 0);
        discover(&mut subscriptions, "reserved", MediaKind::Audio);
        request(&mut subscriptions);
        for index in 1..MAX_RETAINED_PRODUCERS {
            subscriptions.close(&format!("closed-{index}")).unwrap();
        }
        assert!(
            subscriptions
                .discover("overflow".into(), MediaKind::Audio, false)
                .is_err()
        );
        assert_eq!(subscriptions.producers.len(), MAX_RETAINED_PRODUCERS);
        assert_eq!(subscriptions.reserved, [1, 0]);
        assert!(!subscriptions.has_work());
        assert!(
            subscriptions
                .discover("overflow-again".into(), MediaKind::Audio, false)
                .is_ok()
        );
        assert!(subscriptions.close("unretained-close").is_ok());
        assert_eq!(subscriptions.producers.len(), MAX_RETAINED_PRODUCERS);
        subscriptions.close("reserved").unwrap();
        assert_eq!(subscriptions.reserved, [0, 0]);
    }
}
