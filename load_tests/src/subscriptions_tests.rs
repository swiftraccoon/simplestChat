//! Tests of `load_tests/src/subscriptions.rs`.

use super::*;

fn planned(audio: &[usize], video: &[usize]) -> Subscriptions {
    let owners = Arc::new(OwnedParticipants::new(5));
    for index in 0..5 {
        owners
            .register(&format!("participant-{index}"), &format!("client-{index}"))
            .unwrap();
    }
    Subscriptions::with_plan(
        audio.len(),
        video.len(),
        PlannedTargets {
            audio: audio
                .iter()
                .map(|index| format!("client-{index}"))
                .collect(),
            video: video
                .iter()
                .map(|index| format!("client-{index}"))
                .collect(),
        },
        owners,
    )
    .unwrap()
}

fn discover_planned(subscriptions: &mut Subscriptions, owner: usize, id: &str, kind: MediaKind) {
    subscriptions
        .discover_from(&format!("participant-{owner}"), id.to_string(), kind, false)
        .unwrap();
}

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

#[test]
fn planned_graph_preserves_kind_order_for_every_discovery_permutation() {
    let inventory = [
        (1, "a1", MediaKind::Audio),
        (2, "a2", MediaKind::Audio),
        (3, "v3", MediaKind::Video),
        (4, "v4", MediaKind::Video),
    ];
    for first in 0..4 {
        for second in 0..4 {
            for third in 0..4 {
                for fourth in 0..4 {
                    let order = [first, second, third, fourth];
                    if order
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        != 4
                    {
                        continue;
                    }
                    let mut subscriptions = planned(&[1, 2], &[3, 4]);
                    let mut observed = [Vec::new(), Vec::new()];
                    for event in order {
                        let (owner, id, kind) = inventory[event];
                        discover_planned(&mut subscriptions, owner, id, kind);
                        // Duplicate events cannot duplicate or reorder released slots.
                        discover_planned(&mut subscriptions, owner, id, kind);
                        while subscriptions.has_work() {
                            let request = request(&mut subscriptions);
                            observed[kind_index(request.kind)].push(request.producer_id);
                        }
                    }
                    assert_eq!(observed[0], ["a1", "a2"], "{order:?}");
                    assert_eq!(observed[1], ["v3", "v4"], "{order:?}");
                    assert_eq!(subscriptions.reserved, [2, 2]);
                }
            }
        }
    }
}

#[test]
fn missing_planned_peer_blocks_only_its_kind_without_inventory_fallback() {
    let mut subscriptions = planned(&[1, 2], &[3]);
    discover_planned(&mut subscriptions, 2, "a2", MediaKind::Audio);
    discover_planned(&mut subscriptions, 4, "unplanned", MediaKind::Audio);
    assert!(!subscriptions.has_work());
    assert_eq!(subscriptions.reserved, [0, 0]);
    assert_eq!(subscriptions.producers["unplanned"].phase, Phase::Ignored);
    discover_planned(&mut subscriptions, 3, "v3", MediaKind::Video);
    assert_eq!(request(&mut subscriptions).producer_id, "v3");
    assert!(!subscriptions.has_work());
    discover_planned(&mut subscriptions, 1, "a1", MediaKind::Audio);
    assert_eq!(request(&mut subscriptions).producer_id, "a1");
    assert_eq!(request(&mut subscriptions).producer_id, "a2");
    subscriptions.close("a1").unwrap();
    assert!(!subscriptions.has_work());
    assert_eq!(subscriptions.reserved, [1, 1]);
}

#[test]
fn closed_before_discovery_fills_ordered_slot_without_reopening_it() {
    let mut subscriptions = planned(&[1, 2], &[]);
    subscriptions.close("a1").unwrap();
    discover_planned(&mut subscriptions, 2, "a2", MediaKind::Audio);
    assert!(!subscriptions.has_work());
    discover_planned(&mut subscriptions, 1, "a1", MediaKind::Audio);
    assert_eq!(request(&mut subscriptions).producer_id, "a2");
    assert_eq!(subscriptions.producers["a1"].phase, Phase::Closed);
    assert_eq!(subscriptions.reserved, [1, 0]);
    assert!(
        !subscriptions
            .created("a1", MediaKind::Audio, "late")
            .unwrap()
    );
    assert!(!subscriptions.can_resume("late"));
    discover_planned(&mut subscriptions, 1, "a1", MediaKind::Audio);
    assert!(!subscriptions.has_work());
}

#[test]
fn planned_retirement_keeps_existing_reservations_and_has_no_replacements() {
    let mut subscriptions = planned(&[1, 2], &[]);
    discover_planned(&mut subscriptions, 2, "a2", MediaKind::Audio);
    subscriptions
        .discover_from("participant-1", "a1".into(), MediaKind::Audio, true)
        .unwrap();
    assert_eq!(request(&mut subscriptions).producer_id, "a2");
    assert!(
        subscriptions
            .created("a2", MediaKind::Audio, "consumer")
            .unwrap()
    );
    subscriptions
        .discover_from("participant-2", "a2".into(), MediaKind::Audio, true)
        .unwrap();
    assert_eq!(subscriptions.reserved, [1, 0]);
    assert!(subscriptions.can_resume("consumer"));
    discover_planned(&mut subscriptions, 3, "unplanned", MediaKind::Audio);
    subscriptions.close("a2").unwrap();
    assert_eq!(subscriptions.reserved, [0, 0]);
    assert!(!subscriptions.has_work());
}

#[test]
fn planned_conflicts_fail_closed_without_releasing_reserved_slots() {
    for invalid in [
        "unknown-owner",
        "changed-owner",
        "changed-kind",
        "second-producer",
        "ownerless",
    ] {
        let mut subscriptions = planned(&[1, 2], &[1]);
        discover_planned(&mut subscriptions, 1, "a1", MediaKind::Audio);
        assert_eq!(request(&mut subscriptions).producer_id, "a1");
        let result = match invalid {
            "unknown-owner" => {
                subscriptions.discover_from("unregistered", "a2".into(), MediaKind::Audio, false)
            }
            "changed-owner" => {
                subscriptions.discover_from("participant-2", "a1".into(), MediaKind::Audio, false)
            }
            "changed-kind" => {
                subscriptions.discover_from("participant-1", "a1".into(), MediaKind::Video, false)
            }
            "second-producer" => subscriptions.discover_from(
                "participant-1",
                "different".into(),
                MediaKind::Audio,
                false,
            ),
            "ownerless" => subscriptions.discover("a2".into(), MediaKind::Audio, false),
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{invalid}");
        assert!(!subscriptions.has_work(), "{invalid}");
        assert_eq!(subscriptions.reserved, [1, 0], "{invalid}");
    }
}

#[test]
fn ignored_planned_inventory_cannot_change_kind_or_owner() {
    for (owner, kind) in [
        ("participant-4", MediaKind::Audio),
        ("participant-3", MediaKind::Video),
    ] {
        let mut subscriptions = planned(&[1], &[]);
        discover_planned(&mut subscriptions, 3, "unplanned", MediaKind::Audio);
        subscriptions.close("unplanned").unwrap();
        assert!(
            subscriptions
                .discover_from(owner, "unplanned".into(), kind, false)
                .is_err()
        );
        assert!(subscriptions.stopped);
    }
}

#[test]
fn planned_pending_queue_is_bounded_by_targets_not_discovered_inventory() {
    let owners = Arc::new(OwnedParticipants::new(100));
    for index in 0..100 {
        owners
            .register(&format!("participant-{index}"), &format!("client-{index}"))
            .unwrap();
    }
    let targets: Vec<_> = (1..=16).map(|index| format!("client-{index}")).collect();
    let mut subscriptions = Subscriptions::with_plan(
        16,
        16,
        PlannedTargets {
            audio: targets.clone(),
            video: targets,
        },
        owners,
    )
    .unwrap();
    for index in (0..100).rev() {
        for kind in [MediaKind::Audio, MediaKind::Video] {
            discover_planned(
                &mut subscriptions,
                index,
                &format!("{index}-{kind:?}"),
                kind,
            );
            assert!(
                subscriptions
                    .pending
                    .iter()
                    .all(|pending| pending.len() <= 16)
            );
        }
    }
    assert_eq!(subscriptions.pending[0].len(), 16);
    assert_eq!(subscriptions.pending[1].len(), 16);
    assert_eq!(subscriptions.producers.len(), 200);
    assert_eq!(subscriptions.reserved, [0, 0]);
}

#[test]
fn planned_constructor_rejects_duplicate_unknown_and_over_limit_targets() {
    for (limit, targets) in [
        (2, vec!["client-1", "client-1"]),
        (1, vec!["client-2"]),
        (1, vec!["client-01"]),
        (0, vec!["client-1"]),
        (17, vec![]),
    ] {
        let result = Subscriptions::with_plan(
            limit,
            0,
            PlannedTargets {
                audio: targets.into_iter().map(str::to_string).collect(),
                video: Vec::new(),
            },
            Arc::new(OwnedParticipants::new(2)),
        );
        assert!(result.is_err());
    }
}

#[test]
fn fifo_owner_aware_entrypoint_preserves_unowned_discovery_behavior() {
    let mut subscriptions = Subscriptions::new(1, 0);
    subscriptions
        .discover_from("unknown", "audio".into(), MediaKind::Audio, false)
        .unwrap();
    assert_eq!(request(&mut subscriptions).producer_id, "audio");
    assert_eq!(subscriptions.reserved, [1, 0]);
}
