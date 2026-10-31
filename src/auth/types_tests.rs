//! Tests of `src/auth/types.rs` and of every other wire body that carries a
//! credential: passwords, recovery keys, invite codes, access and attachment
//! tokens, the TURN credential, ICE passwords and the push endpoint. The
//! WebSocket ticket response is tested beside its private fields in
//! `src/auth/ws_tickets.rs`.

use super::*;
use crate::attachments::AttachmentAccess;
use crate::auth::account::{
    ChangePasswordRequest, RecoveryKeyRequest, RecoveryKeyResponse, RedeemRecoveryRequest,
};
use crate::auth::invites::{CreatedRegistrationInvite, RegistrationInvite};
use crate::auth::passkeys::{ActionResponse, PasskeyAction, StartRequest};
use crate::ids::fixtures::trid;
use crate::push::SubscriptionRequest;
use crate::room::invites::{CreatedRoomInvite, InviteCodeRequest, RoomInvite};
use crate::room::roles::Role;
use crate::room::settings::CreateRoomRequest;
use crate::signaling::protocol::{ServerMessage, SocialResponse};
use crate::turn::IceServer;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

const SECRET: &str = "hunter2-but-longer-than-fifteen";
/// Account data that the hand-written debug impls leave out.
const ACCOUNT_EMAIL: &str = "private-person@example.test";

/// The value's debug output, which must hide `SECRET` behind the redaction marker.
fn redacted_debug(name: &str, value: &impl std::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    assert!(
        !debug.contains(SECRET),
        "{name} shows a secret in its debug output"
    );
    assert!(
        debug.contains("REDACTED"),
        "{name} does not mark its secret as redacted"
    );
    debug
}

/// A response body still puts the plain secret on the wire at `pointer`.
fn assert_serialized_secret(name: &str, value: &impl serde::Serialize, pointer: &str) {
    let wire = serde_json::to_value(value).unwrap();
    assert!(
        wire.pointer(pointer).and_then(serde_json::Value::as_str) == Some(SECRET),
        "{name} must serialize the plain secret at {pointer}"
    );
}

#[test]
fn debug_output_redacts_every_secret_field() {
    redacted_debug(
        "RegisterRequest",
        &RegisterRequest {
            email: "person@example.test".into(),
            password: SECRET.into(),
            display_name: "Person".into(),
            invite_code: Some(SECRET.into()),
        },
    );
    redacted_debug(
        "LoginRequest",
        &LoginRequest {
            email: "person@example.test".into(),
            password: SECRET.into(),
        },
    );
    redacted_debug(
        "ChangePasswordRequest",
        &ChangePasswordRequest {
            current_password: SECRET.into(),
            new_password: SECRET.into(),
        },
    );
    redacted_debug(
        "RecoveryKeyRequest",
        &RecoveryKeyRequest {
            current_password: SECRET.into(),
        },
    );
    redacted_debug(
        "RedeemRecoveryRequest",
        &RedeemRecoveryRequest {
            email: "person@example.test".into(),
            recovery_key: SECRET.into(),
            new_password: SECRET.into(),
        },
    );
    redacted_debug(
        "StartRequest",
        &StartRequest {
            operation: PasskeyAction::RecoveryKey {},
            current_password: Some(SECRET.into()),
        },
    );
    redacted_debug(
        "CreateRoomRequest",
        &CreateRoomRequest {
            id: "room".into(),
            display_name: "Room".into(),
            password: Some(SECRET.into()),
            require_registration: None,
            max_participants: None,
            max_broadcasters: None,
            moderated: None,
            secret: None,
            lobby_enabled: None,
            guests_allowed: None,
            guests_can_broadcast: None,
            topic: None,
        },
    );
    redacted_debug(
        "InviteCodeRequest",
        &InviteCodeRequest {
            code: SECRET.into(),
        },
    );
    redacted_debug(
        "SubscriptionRequest",
        &serde_json::from_value::<SubscriptionRequest>(json!({ "endpoint": SECRET })).unwrap(),
    );

    // Response bodies redact their debug output and still carry the plain secret.
    let authentication = AuthResponse {
        token: SECRET.into(),
        user: UserInfo {
            id: "account".into(),
            email: "person@example.test".into(),
            display_name: "Person".into(),
        },
    };
    redacted_debug("AuthResponse", &authentication);
    assert_serialized_secret("AuthResponse", &authentication, "/token");
    let recovery = RecoveryKeyResponse {
        recovery_key: SECRET.into(),
    };
    redacted_debug("RecoveryKeyResponse", &recovery);
    assert_serialized_secret("RecoveryKeyResponse", &recovery, "/recovery_key");
    for (name, response) in [
        (
            "ActionResponse::ReplaceRegistration",
            ActionResponse::ReplaceRegistration {
                ceremony_id: "ceremony".into(),
                options: serde_json::Value::Null,
                recovery_key: SECRET.into(),
            },
        ),
        (
            "ActionResponse::RecoveryKey",
            ActionResponse::RecoveryKey {
                recovery_key: SECRET.into(),
            },
        ),
    ] {
        redacted_debug(name, &response);
        assert_serialized_secret(name, &response, "/recovery_key");
    }
    let registration_invite = CreatedRegistrationInvite {
        details: RegistrationInvite {
            id: Uuid::nil(),
            uses_left: 1,
            expires_at: Utc::now(),
            created_at: Utc::now(),
        },
        code: SECRET.into(),
    };
    redacted_debug("CreatedRegistrationInvite", &registration_invite);
    assert_serialized_secret("CreatedRegistrationInvite", &registration_invite, "/code");
    let room_invite = CreatedRoomInvite {
        details: RoomInvite {
            id: Uuid::nil(),
            role: Role::Member,
            uses_left: 1,
            expires_at: Utc::now(),
            created_at: Utc::now(),
        },
        code: SECRET.into(),
    };
    redacted_debug("CreatedRoomInvite", &room_invite);
    assert_serialized_secret("CreatedRoomInvite", &room_invite, "/code");
    let access = AttachmentAccess {
        token: SECRET.into(),
        expires_at: Utc::now(),
    };
    redacted_debug("AttachmentAccess", &access);
    assert_serialized_secret("AttachmentAccess", &access, "/token");
    let socket_access = SocialResponse::GetAttachmentAccess {
        token: SECRET.into(),
        expires_at: Utc::now(),
    };
    redacted_debug("SocialResponse::GetAttachmentAccess", &socket_access);
    assert_serialized_secret(
        "SocialResponse::GetAttachmentAccess",
        &socket_access,
        "/data/token",
    );
    let relay = IceServer {
        urls: vec!["turn:relay.example:3478".into()],
        username: Some("1700000000:relay".into()),
        credential: Some(SECRET.into()),
    };
    redacted_debug("IceServer", &relay);
    assert_serialized_secret("IceServer", &relay, "/credential");

    // The ICE password of a transport arrives with transportCreated and iceRestarted.
    let ice_parameters =
        json!({"usernameFragment": "fragment", "password": SECRET, "iceLite": true});
    let created: ServerMessage = serde_json::from_value(json!({
        "type": "transportCreated",
        "transportId": trid("send"),
        "iceParameters": ice_parameters,
        "iceCandidates": [],
        "dtlsParameters": {"role": "auto", "fingerprints": []},
    }))
    .unwrap();
    redacted_debug("ServerMessage::TransportCreated", &created);
    assert_serialized_secret(
        "ServerMessage::TransportCreated",
        &created,
        "/iceParameters/password",
    );
    let restarted: ServerMessage = serde_json::from_value(json!({
        "type": "iceRestarted",
        "transportId": trid("send"),
        "iceParameters": ice_parameters,
        "iceServers": [{"urls": ["turn:relay.example:3478"], "credential": SECRET}],
    }))
    .unwrap();
    redacted_debug("ServerMessage::IceRestarted", &restarted);
    assert_serialized_secret(
        "ServerMessage::IceRestarted",
        &restarted,
        "/iceParameters/password",
    );
    assert_serialized_secret(
        "ServerMessage::IceRestarted",
        &restarted,
        "/iceServers/0/credential",
    );
}

#[test]
fn debug_output_omits_account_data_from_recovery_and_passkey_bodies() {
    let redemption = RedeemRecoveryRequest {
        email: ACCOUNT_EMAIL.into(),
        recovery_key: SECRET.into(),
        new_password: SECRET.into(),
    };
    assert!(
        !format!("{redemption:?}").contains(ACCOUNT_EMAIL),
        "RedeemRecoveryRequest shows the account email"
    );
    // Passkey options name the account: email, display name and user handle.
    let options = json!({
        "publicKey": {
            "user": {
                "id": ACCOUNT_EMAIL,
                "name": ACCOUNT_EMAIL,
                "displayName": ACCOUNT_EMAIL
            }
        }
    });
    for response in [
        ActionResponse::Authenticate {
            ceremony_id: "ceremony-7".into(),
            options: options.clone(),
        },
        ActionResponse::Register {
            ceremony_id: "ceremony-7".into(),
            options: options.clone(),
        },
        ActionResponse::ReplaceRegistration {
            ceremony_id: "ceremony-7".into(),
            options,
            recovery_key: SECRET.into(),
        },
    ] {
        let debug = format!("{response:?}");
        assert!(
            !debug.contains(ACCOUNT_EMAIL),
            "ActionResponse shows the passkey options"
        );
        assert!(
            debug.contains("ceremony-7"),
            "ActionResponse no longer names its ceremony"
        );
    }
}
