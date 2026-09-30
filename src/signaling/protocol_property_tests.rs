//! Bounded serialization properties. Wire types and application policy are distinct.

use super::{ClientMessage, RequestHeader};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use serde_json::{Value, json};

#[test]
fn boundary_properties_every_operation_has_a_stable_serialized_roundtrip() {
    let inventory: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/security/authorization/operations.json"
    )))
    .unwrap();
    for entry in inventory["websocket"].as_array().unwrap() {
        let message: ClientMessage = serde_json::from_value(entry["body"].clone()).unwrap();
        let canonical = serde_json::to_value(&message).unwrap();
        assert_eq!(canonical["type"], entry["operation"]);
        let decoded: ClientMessage =
            serde_json::from_slice(&serde_json::to_vec(&message).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), canonical);
    }
}

#[test]
fn boundary_properties_chat_text_and_unsigned_sequences_survive_json_escaping() {
    let mut rng = StdRng::seed_from_u64(0x7072_6f74_6f63_6f6c);
    for case in 0..512 {
        let mut text = String::from("\"\\\n\t\0");
        for _ in 0..case % 65 {
            text.push(rng.random::<char>());
        }
        let sequence = match case % 4 {
            0 => 0,
            1 => u64::MAX,
            2 => (1_u64 << 53) - 1,
            _ => rng.random(),
        };
        let message = ClientMessage::ChatMessage {
            content: text.clone(),
            client_message_id: Some(format!("case-{case}")),
            sequence: Some(sequence),
            reply_to: None,
        };
        let encoded = serde_json::to_vec(&message).unwrap();
        assert!(encoded.len() <= 2048, "finite fixture byte budget");
        let ClientMessage::ChatMessage {
            content,
            sequence: decoded_sequence,
            client_message_id,
            ..
        } = serde_json::from_slice(&encoded).unwrap()
        else {
            panic!("chat operation changed in case {case}");
        };
        assert_eq!(content, text);
        assert_eq!(decoded_sequence, Some(sequence));
        assert_eq!(
            client_message_id.as_deref(),
            Some(format!("case-{case}").as_str())
        );
    }
}

#[test]
fn boundary_properties_correlation_bytes_and_lengths_match_the_envelope_contract() {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    for length in 0..=65 {
        let request_id: String = (0..length)
            .map(|index| char::from(ALPHABET[index % ALPHABET.len()]))
            .collect();
        let wire = json!({"type":"createSendTransport", "requestId":request_id}).to_string();
        let decoded = serde_json::from_str::<RequestHeader>(&wire);
        assert_eq!(decoded.is_ok(), (1..=64).contains(&length));
        if let Ok(header) = decoded {
            assert_eq!(header.request_id.as_deref(), Some(request_id.as_str()));
        }
    }
    for byte in 0..=127_u8 {
        let request_id = format!("a{}z", char::from(byte));
        let wire = json!({"requestId":request_id}).to_string();
        assert_eq!(
            serde_json::from_str::<RequestHeader>(&wire).is_ok(),
            ALPHABET.contains(&byte),
            "ASCII byte {byte}"
        );
    }
}

#[test]
fn boundary_properties_integer_fields_reject_values_outside_their_wire_types() {
    for role in 0..=255_u16 {
        let body = json!({"type":"setRole", "targetParticipantId":"target", "role":role});
        let decoded: ClientMessage = serde_json::from_value(body).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap()["role"], json!(role));
    }
    // This checks Serde integer bounds, not the smaller authorized role/layer range.
    for (operation, field, maximum, extra) in [
        (
            "setRole",
            "role",
            u64::from(u8::MAX),
            json!({"targetParticipantId":"target"}),
        ),
        (
            "listRoomMembers",
            "offset",
            u64::from(u32::MAX),
            json!({"requestId":"request"}),
        ),
        (
            "chatMessage",
            "sequence",
            u64::MAX,
            json!({"content":"text"}),
        ),
    ] {
        for invalid in [json!(-1), json!(1.5), json!("1"), json!(true)] {
            let mut body = extra.clone();
            body["type"] = json!(operation);
            body[field] = invalid;
            assert!(serde_json::from_value::<ClientMessage>(body).is_err());
        }
        for value in [0, 1, maximum - 1, maximum] {
            let mut body = extra.clone();
            body["type"] = json!(operation);
            body[field] = json!(value);
            let decoded: ClientMessage = serde_json::from_value(body).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap()[field], json!(value));
        }
        let oversized = format!(
            "{{\"type\":\"{operation}\",\"requestId\":\"request\",\"targetParticipantId\":\"target\",\"content\":\"text\",\"{field}\":{}}}",
            u128::from(maximum) + 1
        );
        assert!(serde_json::from_str::<ClientMessage>(&oversized).is_err());
    }
}

#[test]
fn boundary_properties_nullable_settings_preserve_every_independent_patch_combination() {
    for broadcaster in 0..3 {
        for participants in 0..3 {
            for password in 0..3 {
                let mut body = json!({"type":"updateRoomSettings"});
                for (field, state, value) in [
                    ("maxBroadcasters", broadcaster, json!(12)),
                    ("maxParticipants", participants, json!(30)),
                    ("password", password, json!("synthetic-room-passphrase")),
                ] {
                    if state != 0 {
                        body[field] = if state == 1 { Value::Null } else { value };
                    }
                }
                let decoded: ClientMessage = serde_json::from_value(body.clone()).unwrap();
                let output = serde_json::to_value(decoded).unwrap();
                for field in ["maxBroadcasters", "maxParticipants", "password"] {
                    assert_eq!(output.get(field), body.get(field));
                }
            }
        }
    }
}
