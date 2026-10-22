//! Fixed loopback response and typed-message domains, without networking.

use super::wire::{self, Event};

#[test]
fn readiness_requires_bounded_json_and_exact_ready_status() {
    assert!(wire::ready_body(br#"{"status":"ready"}"#).unwrap());
    for bytes in [
        br#"{"status":"starting"}"#.as_slice(),
        b"PRIVATE".as_slice(),
        br#"{"status":true}"#.as_slice(),
        vec![b' '; 4097].as_slice(),
    ] {
        assert!(wire::ready_body(bytes).is_err());
    }
}

#[test]
fn typed_shutdown_notices_are_exact_and_terminal_admission_messages_refuse() {
    assert!(matches!(
        wire::message(br#"{"type":"serverRestarting","reason":"Server shutting down"}"#).unwrap(),
        Event::Restarting
    ));
    for bytes in [
        br#"{"type":"serverRestarting","reason":"PRIVATE"}"#.as_slice(),
        br#"{"type":"roomClosed","roomId":"test","reason":"closed"}"#.as_slice(),
        br#"{"type":"error","message":"PRIVATE"}"#.as_slice(),
        br#"{"type":"roomPasswordRequired"}"#.as_slice(),
        br#"{"type":"unknown","private":"PRIVATE"}"#.as_slice(),
        b"PRIVATE".as_slice(),
        vec![b' '; 65537].as_slice(),
    ] {
        assert!(wire::message(bytes).is_err());
    }
}
