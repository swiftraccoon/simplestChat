//! Match requested graph identities to complete realized delivery evidence.
//! Native IDs stay private and only join records within this one owned run.

use super::options::{Options, Plan, Scenario};
use crate::PlannedSubscriptionEdge as Edge;
use crate::{PlannedTargets, SubscriptionPlan};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_SAFE: u64 = 9_007_199_254_740_991;

/// Common immutable workload settings, independent of local paths or container
/// limits. Both benchmark backends must submit exactly the requested graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRequest {
    pub subscription_plan: Plan,
    pub subscription_seed: Option<u32>,
    pub duration: u32,
    pub warmup: u32,
    pub ramp_up: u32,
}

impl From<&Options> for GraphRequest {
    fn from(options: &Options) -> Self {
        Self {
            subscription_plan: options.subscription_plan,
            subscription_seed: options.subscription_seed,
            duration: options.duration,
            warmup: options.warmup,
            ramp_up: options.ramp_up,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub subscription_plan: Plan,
    pub subscription_seed: Option<u32>,
    pub subscription_plan_sha256: Option<String>,
}

/// Use the generator's actual immutable graph planner and canonical fingerprint.
/// The benchmark fixes both per-kind caps at four and never invents another
/// ordering from native UUIDs, discovery timing or report order.
pub fn expected(options: &GraphRequest, scenario: &Scenario) -> Result<SubscriptionPlan> {
    let seed = options
        .subscription_seed
        .ok_or_else(|| anyhow::anyhow!("Invalid subscription plan or seed"))?;
    let video = if scenario.audio_only() { 0 } else { 4 };
    match options.subscription_plan {
        Plan::Ring => SubscriptionPlan::ring(
            scenario.clients as usize,
            scenario.rooms as usize,
            4,
            video,
            seed,
        ),
        Plan::Hotspot => SubscriptionPlan::hotspot(
            scenario.clients as usize,
            scenario.rooms as usize,
            4,
            video,
            seed,
        ),
        Plan::Fifo => anyhow::bail!("Invalid subscription plan or seed"),
    }
}

pub fn arguments(options: &GraphRequest) -> Result<Vec<String>> {
    if options.subscription_plan == Plan::Fifo {
        ensure!(
            options.subscription_seed.is_none(),
            "Invalid subscription plan or seed"
        );
        return Ok(Vec::new());
    }
    let seed = options
        .subscription_seed
        .ok_or_else(|| anyhow::anyhow!("Invalid subscription plan or seed"))?;
    Ok(vec![
        "--subscription-plan".to_owned(),
        options.subscription_plan.name().to_owned(),
        "--subscription-seed".to_owned(),
        seed.to_string(),
    ])
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Configuration {
    subscription_plan: Plan,
    subscription_seed: u32,
    num_clients: u32,
    num_rooms: u32,
    publish_ratio: f64,
    room_id: String,
    duration_secs: u32,
    warmup_secs: u32,
    ramp_up_secs: u32,
    churn_rate: f64,
    max_audio_consumers: u32,
    max_video_consumers: u32,
    media_config: MediaConfiguration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaConfiguration {
    audio_enabled: bool,
    video_enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Client {
    client_id: String,
    room_id: String,
    connection_attempts: Vec<Attempt>,
    consumer_delivery: Vec<Delivery>,
}

#[derive(Deserialize)]
struct Attempt {
    coverage: Coverage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Coverage {
    passed: bool,
    skipped_short_tail: bool,
    expected_audio: u32,
    expected_video: u32,
    planned_subscriptions: PlannedCoverage,
}

#[derive(Deserialize)]
struct PlannedCoverage {
    targets: PlannedTargets,
    realized: Vec<Edge>,
    passed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Delivery {
    consumer_id: String,
    producer_id: String,
    is_audio: bool,
    attempt: u32,
    passed: bool,
    skipped_short_lived: bool,
    eligible_seconds: u32,
    seconds_with_packets: u32,
    longest_gap_seconds: u32,
    packets_by_second: Vec<u64>,
}

fn target_kind(targets: &PlannedTargets, audio: bool) -> &[String] {
    if audio {
        &targets.audio
    } else {
        &targets.video
    }
}

fn targets_equal(left: &PlannedTargets, right: &PlannedTargets) -> bool {
    left.audio == right.audio && left.video == right.video
}

/// Project the selected immutable evidence only. The owner already bounded the
/// source file. Extra generator diagnostics stay private and are never returned;
/// a passing boolean alone cannot replace exact per-edge delivery proof.
pub fn verify(
    summary: &Value,
    results: &Value,
    options: &GraphRequest,
    scenario: &Scenario,
) -> Result<Identity> {
    let config = &summary["run"]["configuration"];
    if options.subscription_plan == Plan::Fifo {
        ensure!(
            (config["subscriptionPlan"].is_null() || config["subscriptionPlan"] == "fifo")
                && config["subscriptionSeed"].is_null()
                && summary["subscriptionPlan"].is_null(),
            "Generator subscription plan does not match the requested FIFO workload"
        );
        return Ok(Identity {
            subscription_plan: Plan::Fifo,
            subscription_seed: None,
            subscription_plan_sha256: None,
        });
    }
    let expected = expected(options, scenario)?;
    let config: Configuration = serde_json::from_value(config.clone()).map_err(|_| {
        anyhow::anyhow!("Missing, malformed or mismatched subscription graph report")
    })?;
    let plan: SubscriptionPlan = serde_json::from_value(summary["subscriptionPlan"].clone())
        .map_err(|_| {
            anyhow::anyhow!("Missing, malformed or mismatched subscription graph report")
        })?;
    let clients: Vec<Client> = serde_json::from_value(results.clone())
        .map_err(|_| anyhow::anyhow!("Missing or mismatched subscription graph client"))?;
    ensure!(
        config.subscription_plan == options.subscription_plan
            && Some(config.subscription_seed) == options.subscription_seed
            && config.num_clients == scenario.clients
            && config.num_rooms == scenario.rooms
            && config.publish_ratio == 1.0
            && !config.room_id.is_empty()
            && config.duration_secs == options.duration
            && config.warmup_secs == options.warmup
            && config.ramp_up_secs == options.ramp_up
            && config.churn_rate == 0.0
            && config.max_audio_consumers == 4
            && config.max_video_consumers == 4
            && config.media_config.audio_enabled
            && config.media_config.video_enabled != scenario.audio_only()
            && plan.version == expected.version
            && plan.seed == expected.seed
            && plan.sha256 == expected.sha256
            && plan.clients.len() == expected.clients.len()
            && clients.len() == expected.clients.len(),
        "Missing, malformed or mismatched subscription graph report"
    );
    let clients: BTreeMap<_, _> = clients
        .iter()
        .map(|client| (client.client_id.as_str(), client))
        .collect();
    ensure!(
        clients.len() == expected.clients.len(),
        "Duplicate subscription graph client report"
    );
    let mut producer_owners = BTreeMap::new();
    let mut owner_producers = BTreeMap::new();
    let mut all_consumers = BTreeSet::new();
    for (index, assignment) in expected.clients.iter().enumerate() {
        let reported = &plan.clients[index];
        let client = clients
            .get(assignment.client_id.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing subscription graph client"))?;
        let room = if scenario.rooms == 1 {
            config.room_id.clone()
        } else {
            format!("{}-{}", config.room_id, assignment.room)
        };
        ensure!(
            reported.client_id == assignment.client_id
                && reported.room == assignment.room
                && targets_equal(&reported.targets, &assignment.targets)
                && client.room_id == room
                && client.connection_attempts.len() == 1,
            "Missing or mismatched subscription graph client"
        );
        let coverage = &client.connection_attempts[0].coverage;
        let planned = &coverage.planned_subscriptions;
        ensure!(
            coverage.passed
                && !coverage.skipped_short_tail
                && planned.passed
                && targets_equal(&planned.targets, &assignment.targets)
                && coverage.expected_audio as usize == assignment.targets.audio.len()
                && coverage.expected_video as usize == assignment.targets.video.len()
                && planned.realized.len()
                    == assignment.targets.audio.len() + assignment.targets.video.len(),
            "Incomplete planned subscription coverage"
        );
        let mut targets = BTreeSet::new();
        let mut consumers = BTreeSet::new();
        for edge in &planned.realized {
            let target = (edge.is_audio, edge.publisher_client_id.as_str());
            let deliveries: Vec<_> = client
                .consumer_delivery
                .iter()
                .filter(|delivery| delivery.consumer_id == edge.consumer_id)
                .collect();
            ensure!(
                !edge.consumer_id.is_empty()
                    && !edge.producer_id.is_empty()
                    && !consumers.contains(&edge.consumer_id),
                "Malformed realized subscription proof"
            );
            ensure!(
                producer_owners
                    .get(edge.producer_id.as_str())
                    .is_none_or(|owner| *owner == target)
                    && owner_producers
                        .get(&target)
                        .is_none_or(|producer| *producer == edge.producer_id.as_str())
                    && !all_consumers.contains(&edge.consumer_id),
                "Inconsistent native identity in realized subscription graph"
            );
            ensure!(
                target_kind(&assignment.targets, edge.is_audio).contains(&edge.publisher_client_id)
                    && !targets.contains(&target)
                    && deliveries.len() == 1,
                "Realized subscription does not match its target or delivery proof"
            );
            let delivery = deliveries[0];
            ensure!(
                delivery.producer_id == edge.producer_id
                    && delivery.is_audio == edge.is_audio
                    && delivery.attempt == 1
                    && delivery.passed
                    && !delivery.skipped_short_lived
                    && delivery.eligible_seconds == options.duration
                    && delivery.seconds_with_packets == options.duration
                    && delivery.longest_gap_seconds == 0
                    && delivery.packets_by_second.len() == options.duration as usize
                    && delivery
                        .packets_by_second
                        .iter()
                        .all(|packets| (1..=MAX_SAFE).contains(packets)),
                "Realized subscription does not match its target or delivery proof"
            );
            targets.insert(target);
            consumers.insert(&edge.consumer_id);
            all_consumers.insert(&edge.consumer_id);
            producer_owners.insert(edge.producer_id.as_str(), target);
            owner_producers.insert(target, edge.producer_id.as_str());
        }
        ensure!(
            client.consumer_delivery.len() == consumers.len(),
            "Unexpected consumers outside the planned graph"
        );
    }
    Ok(Identity {
        subscription_plan: options.subscription_plan,
        subscription_seed: options.subscription_seed,
        subscription_plan_sha256: Some(expected.sha256),
    })
}
