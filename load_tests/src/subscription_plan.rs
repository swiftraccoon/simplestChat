//! Binary-side discovery ownership for immutable library graph plans.

use anyhow::{Result, anyhow, ensure};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use simplestchat_load_test::{
    MAX_PLANNED_CLIENTS as MAX_CLIENTS, MAX_PLANNED_TARGETS_PER_KIND as MAX_TARGETS_PER_KIND,
};
pub use simplestchat_load_test::{PlannedTargets, SubscriptionPlan};

#[derive(Debug, Default)]
struct RegisteredParticipants {
    participants: HashMap<String, String>,
    clients: HashSet<String>,
}

/// Exact server-participant to workload-client identity, shared by owned tasks.
/// Register RoomJoined before publishing; unknown discovery is never guessed.
#[derive(Debug)]
pub struct OwnedParticipants {
    expected_clients: usize,
    registered: Mutex<RegisteredParticipants>,
}

impl OwnedParticipants {
    pub fn new(clients: usize) -> Self {
        Self {
            expected_clients: clients,
            registered: Mutex::new(RegisteredParticipants::default()),
        }
    }

    fn valid_size(&self) -> bool {
        (1..=MAX_CLIENTS).contains(&self.expected_clients)
    }

    pub(crate) fn expects_owner(&self, client_id: &str) -> bool {
        self.valid_size()
            && client_id
                .strip_prefix("client-")
                .and_then(|index| index.parse::<usize>().ok())
                .is_some_and(|index| {
                    index < self.expected_clients && client_id == format!("client-{index}")
                })
    }

    /// Reject duplicate registrations, aliases, and conflicting ownership.
    /// This registry intentionally does not support churn or replacement IDs.
    pub fn register(&self, participant_id: &str, client_id: &str) -> Result<()> {
        ensure!(
            self.expects_owner(client_id),
            "Unexpected owned client identity"
        );
        ensure!(
            !participant_id.is_empty() && participant_id.len() <= 128,
            "Invalid owned participant identity"
        );
        let mut registered = self
            .registered
            .lock()
            .map_err(|_| anyhow!("Owned participant registry lock poisoned"))?;
        ensure!(
            !registered.participants.contains_key(participant_id),
            "Owned participant identity is already registered"
        );
        ensure!(
            !registered.clients.contains(client_id),
            "Owned client identity is already registered"
        );
        registered
            .participants
            .insert(participant_id.to_string(), client_id.to_string());
        registered.clients.insert(client_id.to_string());
        Ok(())
    }

    pub fn lookup_owner(&self, participant_id: &str) -> Result<String> {
        self.registered
            .lock()
            .map_err(|_| anyhow!("Owned participant registry lock poisoned"))?
            .participants
            .get(participant_id)
            .cloned()
            .ok_or_else(|| anyhow!("Producer discovery references an unknown owned participant"))
    }
}

#[derive(Debug)]
struct PlannedSlot {
    owner: String,
    producer_id: Option<String>,
}

/// At most 16 slots per kind. Later discovery cannot overtake a missing peer.
#[derive(Debug)]
pub(crate) struct PlanSelection {
    slots: [Vec<PlannedSlot>; 2],
    released: [usize; 2],
}

impl PlanSelection {
    pub(crate) fn new(
        limits: [usize; 2],
        targets: PlannedTargets,
        owners: &OwnedParticipants,
    ) -> Result<Self> {
        ensure!(
            owners.valid_size(),
            "Invalid owned participant registry size"
        );
        let mut slots = [Vec::new(), Vec::new()];
        for (index, kind_targets) in [targets.audio, targets.video].into_iter().enumerate() {
            ensure!(
                limits[index] <= MAX_TARGETS_PER_KIND && kind_targets.len() <= limits[index],
                "Planned targets exceed the per-kind subscription limit"
            );
            let mut seen = HashSet::new();
            for owner in kind_targets {
                ensure!(
                    owners.expects_owner(&owner),
                    "Unexpected planned publisher identity"
                );
                ensure!(
                    seen.insert(owner.clone()),
                    "Duplicate planned publisher for one media kind"
                );
                slots[index].push(PlannedSlot {
                    owner,
                    producer_id: None,
                });
            }
        }
        Ok(Self {
            slots,
            released: [0, 0],
        })
    }

    /// Mark one slot known and release only the newly complete ordered prefix.
    /// An unplanned owner is ordinary inventory, not a replacement candidate.
    pub(crate) fn discover(
        &mut self,
        owner: &str,
        producer_id: &str,
        kind: usize,
    ) -> Result<(bool, Vec<String>)> {
        let Some(slot) = self.slots[kind].iter_mut().find(|slot| slot.owner == owner) else {
            return Ok((false, Vec::new()));
        };
        if let Some(previous) = &slot.producer_id {
            ensure!(
                previous == producer_id,
                "Planned publisher returned conflicting producer IDs"
            );
        } else {
            slot.producer_id = Some(producer_id.to_string());
        }
        let mut ready = Vec::new();
        while let Some(Some(producer)) = self.slots[kind]
            .get(self.released[kind])
            .map(|slot| slot.producer_id.as_ref())
        {
            ready.push(producer.clone());
            self.released[kind] += 1;
        }
        Ok((true, ready))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_matches_independent_sha256_and_json_known_vector() {
        let plan = SubscriptionPlan::ring(6, 2, 4, 4, 17).unwrap();
        assert_eq!(
            plan.sha256,
            "95bb99692aa6be29d2c1e21b3a00ad24e56f91164c4ae6d97c089c921824bfdc"
        );
        for (index, peers) in [[4, 2], [5, 3], [0, 4], [1, 5], [2, 0], [3, 1]]
            .into_iter()
            .enumerate()
        {
            let expected: Vec<_> = peers
                .into_iter()
                .map(|peer| format!("client-{peer}"))
                .collect();
            assert_eq!(plan.targets(index).audio, expected);
            assert_eq!(plan.targets(index).video, expected);
        }
    }

    #[test]
    fn ring_scales_to_the_capacity_ladder_without_leaving_a_room() {
        let plan = SubscriptionPlan::ring(300, 4, 4, 4, 17).unwrap();
        assert_eq!(plan, SubscriptionPlan::ring(300, 4, 4, 4, 17).unwrap());
        assert_eq!(plan.clients.len(), 300);
        let mut incoming = vec![[0usize; 2]; 300];
        for (index, client) in plan.clients.iter().enumerate() {
            assert_eq!(client.room, index % 4);
            for (kind, targets) in [&client.targets.audio, &client.targets.video]
                .into_iter()
                .enumerate()
            {
                assert_eq!(targets.len(), 4);
                for target in targets {
                    let target_index: usize = target.trim_start_matches("client-").parse().unwrap();
                    assert_ne!(target_index, index);
                    assert_eq!(target_index % 4, client.room, "targets stay in the room");
                    incoming[target_index][kind] += 1;
                }
            }
        }
        // The ring spreads fan-out: every publisher serves exactly four per kind.
        assert!(incoming.iter().all(|counts| counts == &[4, 4]));
    }

    #[test]
    fn ring_is_repeatable_seeded_bounded_and_round_trips() {
        let plan = SubscriptionPlan::ring(100, 4, 4, 4, 17).unwrap();
        assert_eq!(plan, SubscriptionPlan::ring(100, 4, 4, 4, 17).unwrap());
        assert_ne!(
            plan.sha256,
            SubscriptionPlan::ring(100, 4, 4, 4, 18).unwrap().sha256
        );
        assert_eq!(plan.version, "ring-v1");
        let encoded = serde_json::to_vec(&plan).unwrap();
        assert_eq!(plan, serde_json::from_slice(&encoded).unwrap());
        let mut incoming = [[0; 100]; 2];
        let mut edges = 0;
        for (index, client) in plan.clients.iter().enumerate() {
            assert_eq!(client.client_id, format!("client-{index}"));
            assert_eq!(client.room, index % 4);
            assert_eq!(plan.targets(index), client.targets);
            for (kind, targets) in [&client.targets.audio, &client.targets.video]
                .into_iter()
                .enumerate()
            {
                assert_eq!(targets.len(), 4);
                let mut unique = HashSet::new();
                for target in targets {
                    let peer: usize = target.strip_prefix("client-").unwrap().parse().unwrap();
                    assert_ne!(peer, index);
                    assert_eq!(peer % 4, index % 4);
                    assert!(unique.insert(peer));
                    incoming[kind][peer] += 1;
                    edges += 1;
                }
            }
        }
        assert_eq!(edges, 800);
        assert!(incoming.into_iter().flatten().all(|count| count == 4));
    }

    #[test]
    fn small_and_uneven_rooms_clamp_targets_without_crossing_rooms() {
        let plan = SubscriptionPlan::ring(7, 3, 16, 0, u32::MAX).unwrap();
        for (index, client) in plan.clients.iter().enumerate() {
            let expected = (0..7)
                .filter(|peer| *peer != index && peer % 3 == index % 3)
                .count();
            assert_eq!(client.targets.audio.len(), expected);
            assert!(client.targets.video.is_empty());
            assert!(client.targets.audio.iter().all(|target| {
                target
                    .strip_prefix("client-")
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
                    % 3
                    == index % 3
            }));
        }
        let singleton = SubscriptionPlan::ring(1, 1, 16, 16, 0).unwrap();
        assert!(singleton.targets(0).audio.is_empty());
        assert!(singleton.targets(0).video.is_empty());
        let separate = SubscriptionPlan::ring(100, 100, 16, 16, 0).unwrap();
        assert!(
            separate
                .clients
                .iter()
                .all(|client| client.targets.audio.is_empty() && client.targets.video.is_empty())
        );
    }

    #[test]
    fn ring_rejects_unbounded_or_undefined_inputs() {
        for (clients, rooms, audio, video) in [
            (0, 1, 1, 1),
            (401, 1, 1, 1),
            (2, 0, 1, 1),
            (2, 3, 1, 1),
            (2, 1, 17, 0),
            (2, 1, 0, 17),
        ] {
            assert!(SubscriptionPlan::ring(clients, rooms, audio, video, 0).is_err());
        }
    }

    #[test]
    fn hotspot_matches_independent_sha256_and_json_known_vector() {
        let plan = SubscriptionPlan::hotspot(5, 2, 4, 4, 17).unwrap();
        assert_eq!(plan.version, "hotspot-v1");
        assert_eq!(plan.seed, 17);
        assert_eq!(
            plan.sha256,
            "c0a2a4f1efa8664e5f283a53a4fb46a86880985b3d129534e4c170908bb41317"
        );
        for (index, peers) in [vec![2, 4], vec![3], vec![0, 4], vec![1], vec![0, 2]]
            .into_iter()
            .enumerate()
        {
            let expected: Vec<_> = peers
                .into_iter()
                .map(|peer| format!("client-{peer}"))
                .collect();
            assert_eq!(plan.targets(index).audio, expected);
            assert_eq!(plan.targets(index).video, expected);
        }
    }

    #[test]
    fn hotspot_concentrates_exact_fanout_on_early_hubs() {
        let plan = SubscriptionPlan::hotspot(100, 4, 4, 4, 17).unwrap();
        assert_eq!(
            plan.sha256,
            "d5879adc2be169085e765384b14f3ac90b1f2563e51d55553990eb84dd9a352d"
        );
        let ranked_hubs = [
            [0, 8, 12, 16, 4],
            [5, 13, 1, 9, 17],
            [6, 18, 2, 14, 10],
            [11, 7, 15, 19, 3],
        ];
        let mut incoming = [[0; 100]; 2];
        let mut edges = 0;
        for (index, client) in plan.clients.iter().enumerate() {
            assert_eq!(client.client_id, format!("client-{index}"));
            assert_eq!(client.room, index % 4);
            assert_eq!(plan.targets(index), client.targets);
            let expected: Vec<_> = ranked_hubs[client.room]
                .into_iter()
                .filter(|peer| *peer != index)
                .take(4)
                .map(|peer| format!("client-{peer}"))
                .collect();
            for (kind, targets) in [&client.targets.audio, &client.targets.video]
                .into_iter()
                .enumerate()
            {
                assert_eq!(targets, &expected);
                assert_eq!(targets.len(), 4);
                let mut unique = HashSet::new();
                for target in targets {
                    let peer: usize = target.strip_prefix("client-").unwrap().parse().unwrap();
                    assert_ne!(peer, index);
                    assert_eq!(peer % 4, index % 4);
                    assert!(peer < 20, "Only the first five members per room are hubs");
                    assert!(unique.insert(peer));
                    incoming[kind][peer] += 1;
                    edges += 1;
                }
            }
        }
        assert_eq!(edges, 800);
        for kind in incoming {
            for hubs in ranked_hubs {
                for peer in &hubs[..4] {
                    assert_eq!(kind[*peer], 24);
                }
                assert_eq!(kind[hubs[4]], 4);
            }
            assert!(kind[20..].iter().all(|count| *count == 0));
        }
    }

    #[test]
    fn hotspot_is_repeatable_seeded_and_round_trips() {
        let plan = SubscriptionPlan::hotspot(100, 4, 4, 4, 17).unwrap();
        assert_eq!(plan, SubscriptionPlan::hotspot(100, 4, 4, 4, 17).unwrap());
        let alternate = SubscriptionPlan::hotspot(100, 4, 4, 4, 18).unwrap();
        assert_ne!(plan.sha256, alternate.sha256);
        assert_ne!(plan.clients, alternate.clients);
        let encoded = serde_json::to_vec(&plan).unwrap();
        assert_eq!(plan, serde_json::from_slice(&encoded).unwrap());
        assert_ne!(
            plan.sha256,
            SubscriptionPlan::ring(100, 4, 4, 4, 17).unwrap().sha256
        );
    }

    #[test]
    fn hotspot_media_kinds_take_independent_prefixes_after_self_exclusion() {
        let plan = SubscriptionPlan::hotspot(8, 1, 2, 4, 17).unwrap();
        assert_eq!(
            plan.sha256,
            "545e23805c813551703f1d8ea79057d856527959c3e6e9ed0e9ded6f53b3dfef"
        );
        for (index, client) in plan.clients.iter().enumerate() {
            let expected: Vec<_> = (0..5)
                .filter(|peer| *peer != index)
                .take(4)
                .map(|peer| format!("client-{peer}"))
                .collect();
            assert_eq!(client.targets.video, expected);
            assert_eq!(client.targets.audio, expected[..2]);
        }
        let swapped = SubscriptionPlan::hotspot(8, 1, 4, 2, 17).unwrap();
        for (client, swapped_client) in plan.clients.iter().zip(swapped.clients) {
            assert_eq!(client.targets.audio, swapped_client.targets.video);
            assert_eq!(client.targets.video, swapped_client.targets.audio);
        }
    }

    #[test]
    fn hotspot_clamps_small_uneven_and_singleton_rooms_and_disabled_media() {
        let plan = SubscriptionPlan::hotspot(7, 3, 16, 0, u32::MAX).unwrap();
        for (index, client) in plan.clients.iter().enumerate() {
            let expected: HashSet<_> = (0..7)
                .filter(|peer| *peer != index && peer % 3 == index % 3)
                .map(|peer| format!("client-{peer}"))
                .collect();
            assert_eq!(client.targets.audio.len(), expected.len());
            assert_eq!(
                client.targets.audio.iter().cloned().collect::<HashSet<_>>(),
                expected
            );
            assert!(client.targets.video.is_empty());
        }
        let video_only = SubscriptionPlan::hotspot(7, 3, 0, 16, u32::MAX).unwrap();
        for (client, video_client) in plan.clients.iter().zip(video_only.clients) {
            assert!(video_client.targets.audio.is_empty());
            assert_eq!(video_client.targets.video, client.targets.audio);
        }
        for plan in [
            SubscriptionPlan::hotspot(1, 1, 16, 16, 0).unwrap(),
            SubscriptionPlan::hotspot(100, 100, 16, 16, 0).unwrap(),
            SubscriptionPlan::hotspot(100, 4, 0, 0, 0).unwrap(),
        ] {
            assert!(
                plan.clients.iter().all(
                    |client| client.targets.audio.is_empty() && client.targets.video.is_empty()
                )
            );
        }
        let maximum = SubscriptionPlan::hotspot(100, 1, 16, 16, 0).unwrap();
        assert!(maximum.clients.iter().all(|client| {
            client.targets.audio.len() == 16
                && client.targets.video.len() == 16
                && client.targets.audio == client.targets.video
        }));
    }

    #[test]
    fn hotspot_rejects_unbounded_or_undefined_inputs_before_index_arithmetic() {
        for (clients, rooms, audio, video) in [
            (0, 1, 1, 1),
            (401, 1, 1, 1),
            (usize::MAX, 1, 1, 1),
            (2, 0, 1, 1),
            (2, 3, 1, 1),
            (2, usize::MAX, 1, 1),
            (2, 1, 17, 0),
            (2, 1, 0, 17),
            (2, 1, usize::MAX, 0),
            (2, 1, 0, usize::MAX),
        ] {
            assert!(SubscriptionPlan::hotspot(clients, rooms, audio, video, 0).is_err());
        }
    }

    #[test]
    fn owned_registry_rejects_aliases_duplicates_and_conflicts_without_mutation() {
        let registry = OwnedParticipants::new(2);
        assert!(registry.lookup_owner("participant-a").is_err());
        for client in ["client-2", "client-01", "client-+1", "1", "Client-1"] {
            assert!(registry.register("participant-a", client).is_err());
        }
        assert!(registry.register("", "client-0").is_err());
        assert!(registry.register(&"x".repeat(129), "client-0").is_err());
        registry.register("participant-a", "client-0").unwrap();
        assert!(registry.register("participant-a", "client-0").is_err());
        assert!(registry.register("participant-a", "client-1").is_err());
        assert!(registry.register("participant-b", "client-0").is_err());
        registry.register("participant-b", "client-1").unwrap();
        assert_eq!(registry.lookup_owner("participant-a").unwrap(), "client-0");
        assert_eq!(registry.lookup_owner("participant-b").unwrap(), "client-1");
        assert!(OwnedParticipants::new(0).register("p", "client-0").is_err());
        assert!(
            OwnedParticipants::new(401)
                .register("p", "client-0")
                .is_err()
        );
    }
}
