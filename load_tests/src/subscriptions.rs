// Bounded, per-attempt subscription scheduling for owned synthetic clients.
// Retired inventory is filtered before sending a new Consume. Retirement is
// not permission to evict an existing native consumer or release its slot.

use anyhow::{Result, anyhow};
use mediasoup::prelude::MediaKind;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::subscription_plan::{OwnedParticipants, PlanSelection, PlannedTargets};

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
    owner: Option<String>,
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
    plan: Option<(PlanSelection, Arc<OwnedParticipants>)>,
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
            plan: None,
        }
    }

    /// Use a fixed owned graph, retaining the same pacing and reservation rules
    /// as FIFO. Missing publishers are never replaced with incidental inventory.
    pub fn with_plan(
        max_audio: usize,
        max_video: usize,
        targets: PlannedTargets,
        owners: Arc<OwnedParticipants>,
    ) -> Result<Self> {
        let selection = PlanSelection::new([max_audio, max_video], targets, &owners)?;
        let mut subscriptions = Self::new(max_audio, max_video);
        subscriptions.plan = Some((selection, owners));
        Ok(subscriptions)
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
        if self.plan.is_some() && !self.stopped {
            return self.fail("Planned subscription discovery requires authoritative ownership");
        }
        self.discover_inventory(producer_id, kind, retired, true, true)
    }

    /// Match exact RoomJoined identities before scheduling planned inventory.
    /// Discoveries may fill later bounded slots, but only a complete canonical
    /// prefix enters FIFO. Unknown or conflicting identities stop dispatch.
    pub fn discover_from(
        &mut self,
        participant_id: &str,
        producer_id: String,
        kind: MediaKind,
        retired: bool,
    ) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let Some((selection, owners)) = &mut self.plan else {
            return self.discover(producer_id, kind, retired);
        };
        let owner = match owners.lookup_owner(participant_id) {
            Ok(owner) => owner,
            Err(error) => return self.fail(&error.to_string()),
        };
        if self.producers.get(&producer_id).is_some_and(|producer| {
            producer
                .owner
                .as_ref()
                .is_some_and(|previous| previous != &owner)
                || producer.kind.is_some_and(|previous| previous != kind)
        }) {
            return self.fail("Producer discovery changed authoritative ownership or media kind");
        }
        let index = kind_index(kind);
        let (selected, ready) = match selection.discover(&owner, &producer_id, index) {
            Ok(discovery) => discovery,
            Err(error) => return self.fail(&error.to_string()),
        };
        self.discover_inventory(producer_id.clone(), kind, retired, selected, false)?;
        self.producers
            .get_mut(&producer_id)
            .expect("Successful discovery retains its inventory")
            .owner = Some(owner);
        for ready_id in ready {
            if self
                .producers
                .get(&ready_id)
                .is_some_and(|producer| producer.phase == Phase::Pending)
            {
                self.pending[index].push_back(ready_id);
            }
        }
        Ok(())
    }

    fn discover_inventory(
        &mut self,
        producer_id: String,
        kind: MediaKind,
        retired: bool,
        selected: bool,
        enqueue: bool,
    ) -> Result<()> {
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
        let phase = if retired || self.limits[index] == 0 || !selected {
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
                owner: None,
            },
        );
        if phase == Phase::Pending && enqueue {
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
                owner: None,
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
#[path = "subscriptions_tests.rs"]
mod tests;
