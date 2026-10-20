//! Tests of `src/signaling/connection_gates.rs`: the per-connection budgets a
//! client command passes before dispatch.

use super::*;

fn command(json: serde_json::Value) -> ClientMessage {
    serde_json::from_value(json).expect("fixture command")
}

fn pause_producer() -> ClientMessage {
    command(serde_json::json!({
        "type": "pauseProducer",
        "producerId": "3d4d3fcb-5e7f-4a3b-9f3e-1f6c7c8d9e0a"
    }))
}

#[test]
fn media_mutations_are_refused_then_close_the_socket_after_repeated_violations() {
    let now = Instant::now();
    let mut gates = CommandGates::new(now, 1);
    for _ in 0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS {
        assert!(matches!(
            gates.admit(&pause_producer(), now),
            Admission::Allowed
        ));
    }
    for _ in 1..MEDIA_MUTATION_MAX_VIOLATIONS {
        assert!(matches!(
            gates.admit(&pause_producer(), now),
            Admission::Refused(ServerMessage::Error {
                code: ErrorCode::RateLimited,
                ..
            })
        ));
    }
    assert!(matches!(
        gates.admit(&pause_producer(), now),
        Admission::Close {
            why: CloseReason::MediaMutations(MEDIA_MUTATION_MAX_VIOLATIONS),
            ..
        }
    ));
}

#[test]
fn a_chat_flood_closes_the_socket_at_once() {
    let now = Instant::now();
    let mut gates = CommandGates::new(now, 1);
    let chat = || command(serde_json::json!({"type": "chatMessage", "content": "hello"}));
    for _ in 0..CHAT_RATE_LIMIT_MAX_TOKENS {
        assert!(matches!(gates.admit(&chat(), now), Admission::Allowed));
    }
    assert!(matches!(
        gates.admit(&chat(), now),
        Admission::Close {
            why: CloseReason::ChatFlood,
            ..
        }
    ));
}

#[test]
fn voice_requests_wait_for_their_cooldown() {
    let now = Instant::now();
    let mut gates = CommandGates::new(now, 1);
    assert!(matches!(
        gates.admit(&ClientMessage::RequestVoice, now),
        Admission::Allowed
    ));
    assert!(matches!(
        gates.admit(&ClientMessage::RequestVoice, now + Duration::from_secs(1)),
        Admission::Refused(_)
    ));
    assert!(matches!(
        gates.admit(&ClientMessage::RequestVoice, now + VOICE_REQUEST_COOLDOWN),
        Admission::Allowed
    ));
}

#[test]
fn administration_changes_are_refused_then_close_the_socket() {
    let now = Instant::now();
    let mut gates = CommandGates::new(now, 1);
    let topic = || command(serde_json::json!({"type": "setTopic", "topic": "Agenda"}));
    for _ in 0..ADMIN_MUTATION_RATE_LIMIT_MAX_TOKENS {
        assert!(matches!(gates.admit(&topic(), now), Admission::Allowed));
    }
    for _ in 1..ADMIN_MUTATION_MAX_VIOLATIONS {
        assert!(matches!(gates.admit(&topic(), now), Admission::Refused(_)));
    }
    assert!(matches!(
        gates.admit(&topic(), now),
        Admission::Close {
            why: CloseReason::AdminMutations(ADMIN_MUTATION_MAX_VIOLATIONS),
            ..
        }
    ));
}

#[test]
fn a_reconnect_continues_the_media_budget_and_a_join_renews_it() {
    let now = Instant::now();
    let mut gates = CommandGates::new(now, 1);
    for _ in 0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS {
        assert!(matches!(
            gates.admit(&pause_producer(), now),
            Admission::Allowed
        ));
    }
    let spent = gates.into_media_state();
    let mut gates = CommandGates::new(now, 1);
    gates.restore_media(spent);
    assert!(matches!(
        gates.admit(&pause_producer(), now),
        Admission::Refused(_)
    ));
    gates.reset_media(now, 1);
    assert!(matches!(
        gates.admit(&pause_producer(), now),
        Admission::Allowed
    ));
}
