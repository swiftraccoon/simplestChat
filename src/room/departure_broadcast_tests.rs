use super::*;
use std::cell::Cell;

fn metric(metrics: &ServerMetrics, name: &str) -> u64 {
    metrics
        .render_prometheus(0, 0, 0)
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name} ")))
        .unwrap()
        .parse()
        .unwrap()
}

fn participant(id: &str, sender: mpsc::Sender<Arc<String>>) -> Participant {
    Participant {
        id: id.into(),
        name: id.into(),
        sender,
        social: social::ParticipantSocial::new(0),
        media_session_id: uuid::Uuid::new_v4(),
        producers: HashMap::new(),
        role: roles::Role::Guest,
        punitive: moderation::PunitiveState::default(),
        authenticated: false,
        ip: None,
    }
}

fn departure() -> serde_json::Result<String> {
    serde_json::to_string(&ServerMessage::ParticipantLeft {
        participant_id: "departed".into(),
    })
}

#[test]
fn departure_payload_is_not_constructed_without_recipients() {
    let metrics = ServerMetrics::new();
    let constructed = Cell::new(0);
    let before = metrics.render_prometheus(0, 0, 0);
    broadcast_departure(&metrics, std::iter::empty(), || {
        constructed.set(constructed.get() + 1);
        departure()
    });
    // The factory constructs the participant-ID/JSON Strings before the helper
    // creates their shared Arc. Not invoking it proves no payload was allocated.
    assert_eq!(constructed.get(), 0);
    assert_eq!(metrics.render_prometheus(0, 0, 0), before);
}

#[test]
fn departure_skips_known_closed_channels_before_constructing_payload_or_enqueuing() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(1);
    let queued = Arc::new(String::from("already queued"));
    sender.try_send(queued.clone()).unwrap();
    receiver.close();
    let constructed = Cell::new(0);
    let before = metrics.render_prometheus(0, 0, 0);
    broadcast_departure(&metrics, std::iter::once(&sender), || {
        constructed.set(constructed.get() + 1);
        departure()
    });
    assert_eq!(constructed.get(), 0);
    assert_eq!(metrics.render_prometheus(0, 0, 0), before);
    assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &queued));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn departure_serializes_once_and_shares_only_with_live_recipients() {
    let metrics = ServerMetrics::new();
    let (closed, mut closed_receiver) = mpsc::channel(1);
    let (first, mut first_receiver) = mpsc::channel(1);
    let (second, mut second_receiver) = mpsc::channel(1);
    closed_receiver.close();
    let constructed = Cell::new(0);
    let before = metrics.render_prometheus(0, 0, 0);
    broadcast_departure(
        &metrics,
        [&closed, &first, &closed, &second].into_iter(),
        || {
            constructed.set(constructed.get() + 1);
            departure()
        },
    );
    assert_eq!(constructed.get(), 1);
    let first_message = first_receiver.try_recv().unwrap();
    let second_message = second_receiver.try_recv().unwrap();
    assert!(Arc::ptr_eq(&first_message, &second_message));
    assert_eq!(Arc::strong_count(&first_message), 2);
    assert_eq!(first_message.as_str(), departure().unwrap());
    assert!(first_receiver.try_recv().is_err());
    assert!(second_receiver.try_recv().is_err());
    assert_eq!(closed_receiver.len(), 0);
    assert_eq!(metrics.render_prometheus(0, 0, 0), before);
}

#[test]
fn departure_preserves_full_queue_accounting_and_other_live_delivery() {
    let metrics = ServerMetrics::new();
    let (full, mut full_receiver) = mpsc::channel(1);
    let (closed, mut closed_receiver) = mpsc::channel(1);
    let (live, mut live_receiver) = mpsc::channel(1);
    full.try_send(Arc::new("existing".into())).unwrap();
    closed_receiver.close();
    broadcast_departure(&metrics, [&full, &closed, &live].into_iter(), departure);
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_full_total"),
        1
    );
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_closed_total"),
        0
    );
    assert_eq!(full_receiver.try_recv().unwrap().as_str(), "existing");
    assert_eq!(
        live_receiver.try_recv().unwrap().as_str(),
        departure().unwrap()
    );
    assert_eq!(closed_receiver.len(), 0);
}

#[test]
fn departure_counts_a_channel_that_closes_after_eligibility_was_checked() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(1);
    broadcast_departure(&metrics, std::iter::once(&sender), || {
        // Payload construction happens after selecting the first live sender.
        // This deterministically exercises the unavoidable close-before-send race.
        receiver.close();
        departure()
    });
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_closed_total"),
        1
    );
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_full_total"),
        0
    );
    assert_eq!(receiver.len(), 0);
}

#[test]
fn departure_fanout_does_not_remove_grace_memberships_or_replace_authoritative_senders() {
    let mut room = Room::new("room".into(), "router".into(), None, false, None);
    let (closed, mut closed_receiver) = mpsc::channel(1);
    let (live, mut live_receiver) = mpsc::channel(1);
    closed_receiver.close();
    room.participants
        .insert("grace".into(), participant("grace", closed.clone()));
    room.participants
        .insert("live".into(), participant("live", live.clone()));
    room.social.next_sequence = 42;
    let grace_session = room.participants["grace"].media_session_id;
    let live_session = room.participants["live"].media_session_id;
    room.broadcast_participant_left("departed");
    assert_eq!(room.participants.len(), 2);
    assert!(room.participants["grace"].sender.same_channel(&closed));
    assert!(room.participants["live"].sender.same_channel(&live));
    assert_eq!(room.participants["grace"].media_session_id, grace_session);
    assert_eq!(room.participants["live"].media_session_id, live_session);
    assert_eq!(room.social.next_sequence, 42);
    assert_eq!(
        live_receiver.try_recv().unwrap().as_str(),
        departure().unwrap()
    );
    assert_eq!(
        metric(&room.metrics, "simplestchat_outbound_queue_closed_total"),
        0
    );

    // The specialization must not weaken essential room-control or direct replies.
    room.broadcast_all(&ServerMessage::RoomClosed {
        reason: "owned test".into(),
    });
    assert_eq!(
        metric(&room.metrics, "simplestchat_outbound_queue_closed_total"),
        1
    );
    assert!(matches!(
        try_send_essential(&room.metrics, &closed, Arc::new("direct reply".into())),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
    assert_eq!(
        metric(&room.metrics, "simplestchat_outbound_queue_closed_total"),
        2
    );
}

#[test]
fn simultaneous_grace_departures_do_not_attempt_quadratic_closed_queue_fanout() {
    let metrics = ServerMetrics::new();
    for room_index in 0..4 {
        let mut room = Room::new(
            format!("room-{room_index}"),
            "router".into(),
            None,
            false,
            None,
        );
        room.metrics = metrics.clone();
        for index in 0..25 {
            let (sender, receiver) = mpsc::channel(1);
            drop(receiver);
            let id = format!("participant-{index}");
            room.participants
                .insert(id.clone(), participant(&id, sender));
        }
        for index in 0..25 {
            let id = format!("participant-{index}");
            assert!(room.participants.remove(&id).is_some());
            room.broadcast_participant_left(&id);
            assert_eq!(room.participants.len(), 24 - index);
        }
    }
    // The former unconditional loop made 4 * 25 * 24 / 2 = 1,200 rejected sends.
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_closed_total"),
        0
    );
    assert_eq!(
        metric(&metrics, "simplestchat_outbound_queue_full_total"),
        0
    );
}
