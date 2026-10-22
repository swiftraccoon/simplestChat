// Reproducible, bounded subscription graphs for owned synthetic clients.
// Numeric workload identities define the graph; server UUIDs and discovery
// timing are deliberately excluded from both selection and its fingerprint.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bound shared by graph planning and the owned participant registry.
pub const MAX_PLANNED_CLIENTS: usize = 400;
/// Maximum directed subscriptions per kind in a planned workload.
pub const MAX_PLANNED_TARGETS_PER_KIND: usize = 16;
const VERSION: &str = "ring-v1";
const PERMUTATION_DOMAIN: &[u8] = b"simplestchat:ring-v1:permutation\0";
const HOTSPOT_VERSION: &str = "hotspot-v1";
const HOTSPOT_PERMUTATION_DOMAIN: &[u8] = b"simplestchat:hotspot-v1:permutation\0";

/// Ordered publisher identities that one client must actually consume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTargets {
    pub audio: Vec<String>,
    pub video: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedClient {
    pub client_id: String,
    pub room: usize,
    pub targets: PlannedTargets,
}

/// Versioned graph and a fingerprint of its canonical, UUID-free metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionPlan {
    pub version: String,
    pub seed: u32,
    pub sha256: String,
    pub clients: Vec<PlannedClient>,
}

#[derive(Serialize)]
struct Fingerprint<'a> {
    version: &'a str,
    seed: u32,
    clients: &'a [PlannedClient],
}

impl SubscriptionPlan {
    /// Assign each room a seed-dependent ring and consume its next K peers.
    ///
    /// Room membership is `numeric_client_index % rooms`. Ring ordering is
    /// lexicographic SHA-256 of the ASCII permutation domain (including its
    /// trailing NUL), then seed, room, and client index as big-endian u32s.
    /// Digest ties use numeric index. Each kind takes the first K successors,
    /// clamped to the room's other members, so no client consumes itself.
    ///
    /// The graph fingerprint hashes compact UTF-8 JSON with field order
    /// `version, seed, clients`; each client uses `clientId, room, targets`,
    /// and targets use `audio, video`. Clients are in numeric index order.
    /// No random state, platform-sized integer bytes, or server IDs enter it.
    pub fn ring(
        clients: usize,
        rooms: usize,
        audio_limit: usize,
        video_limit: usize,
        seed: u32,
    ) -> Result<Self> {
        ensure!(
            (1..=MAX_PLANNED_CLIENTS).contains(&clients),
            "Planned subscription clients must be between 1 and {MAX_PLANNED_CLIENTS}"
        );
        ensure!(
            (1..=clients).contains(&rooms),
            "Planned subscription rooms must be between 1 and the client count"
        );
        ensure!(
            audio_limit <= MAX_PLANNED_TARGETS_PER_KIND
                && video_limit <= MAX_PLANNED_TARGETS_PER_KIND,
            "Planned subscription limits must not exceed {MAX_PLANNED_TARGETS_PER_KIND} per kind"
        );
        let mut planned: Vec<_> = (0..clients)
            .map(|index| PlannedClient {
                client_id: format!("client-{index}"),
                room: index % rooms,
                targets: PlannedTargets {
                    audio: Vec::new(),
                    video: Vec::new(),
                },
            })
            .collect();
        for room in 0..rooms {
            let mut ring: Vec<_> = (room..clients)
                .step_by(rooms)
                .map(|index| {
                    let mut digest = Sha256::new();
                    digest.update(PERMUTATION_DOMAIN);
                    digest.update(seed.to_be_bytes());
                    digest.update(
                        u32::try_from(room)
                            .expect("Room count is bounded")
                            .to_be_bytes(),
                    );
                    digest.update(
                        u32::try_from(index)
                            .expect("Client count is bounded")
                            .to_be_bytes(),
                    );
                    (digest.finalize(), index)
                })
                .collect();
            ring.sort_unstable();
            let peers = ring.len() - 1;
            for (position, (_, index)) in ring.iter().enumerate() {
                let targets = |limit: usize| {
                    (1..=limit.min(peers))
                        .map(|offset| {
                            format!("client-{}", ring[(position + offset) % ring.len()].1)
                        })
                        .collect()
                };
                planned[*index].targets = PlannedTargets {
                    audio: targets(audio_limit),
                    video: targets(video_limit),
                };
            }
        }
        let sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&Fingerprint {
            version: VERSION,
            seed,
            clients: &planned,
        })?));
        Ok(Self {
            version: VERSION.to_string(),
            seed,
            sha256,
            clients: planned,
        })
    }

    /// Concentrate each room's subscribers on an early, seeded publisher pool.
    ///
    /// Room membership is `numeric_client_index % rooms`. The hub pool contains
    /// its first `max(audio_limit, video_limit) + 1` numeric members, clamped to
    /// room size, so the busiest publishers join early in a staggered ramp.
    /// Pool ordering hashes the hotspot permutation domain (including NUL),
    /// then seed, room, and client index as big-endian u32s, with numeric index
    /// breaking digest ties. Each kind takes its first K hubs excluding self.
    /// The extra hub ensures even hub subscribers can fill their target count.
    ///
    /// Fingerprint field order and client identity conventions match `ring`;
    /// the distinct version and permutation domain keep the graphs separate.
    pub fn hotspot(
        clients: usize,
        rooms: usize,
        audio_limit: usize,
        video_limit: usize,
        seed: u32,
    ) -> Result<Self> {
        ensure!(
            (1..=MAX_PLANNED_CLIENTS).contains(&clients),
            "Planned subscription clients must be between 1 and {MAX_PLANNED_CLIENTS}"
        );
        ensure!(
            (1..=clients).contains(&rooms),
            "Planned subscription rooms must be between 1 and the client count"
        );
        ensure!(
            audio_limit <= MAX_PLANNED_TARGETS_PER_KIND
                && video_limit <= MAX_PLANNED_TARGETS_PER_KIND,
            "Planned subscription limits must not exceed {MAX_PLANNED_TARGETS_PER_KIND} per kind"
        );
        let mut planned: Vec<_> = (0..clients)
            .map(|index| PlannedClient {
                client_id: format!("client-{index}"),
                room: index % rooms,
                targets: PlannedTargets {
                    audio: Vec::new(),
                    video: Vec::new(),
                },
            })
            .collect();
        for room in 0..rooms {
            let mut hubs: Vec<_> = (room..clients)
                .step_by(rooms)
                .take(audio_limit.max(video_limit) + 1)
                .map(|index| {
                    let mut digest = Sha256::new();
                    digest.update(HOTSPOT_PERMUTATION_DOMAIN);
                    digest.update(seed.to_be_bytes());
                    digest.update(
                        u32::try_from(room)
                            .expect("Room count is bounded")
                            .to_be_bytes(),
                    );
                    digest.update(
                        u32::try_from(index)
                            .expect("Client count is bounded")
                            .to_be_bytes(),
                    );
                    (digest.finalize(), index)
                })
                .collect();
            hubs.sort_unstable();
            for index in (room..clients).step_by(rooms) {
                let targets = |limit: usize| {
                    hubs.iter()
                        .filter(|(_, peer)| *peer != index)
                        .take(limit)
                        .map(|(_, peer)| format!("client-{peer}"))
                        .collect()
                };
                planned[index].targets = PlannedTargets {
                    audio: targets(audio_limit),
                    video: targets(video_limit),
                };
            }
        }
        let sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&Fingerprint {
            version: HOTSPOT_VERSION,
            seed,
            clients: &planned,
        })?));
        Ok(Self {
            version: HOTSPOT_VERSION.to_string(),
            seed,
            sha256,
            clients: planned,
        })
    }

    /// Return the immutable graph assignment for a validated workload index.
    pub fn targets(&self, index: usize) -> PlannedTargets {
        self.clients[index].targets.clone()
    }
}
