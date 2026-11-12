//! Sign the retained owner in, creating the account once; create the lobby
//! only when the owner has none; end the session whatever happened.

use crate::client::{Api, Operation};
use crate::json::Json;
use crate::{Limits, TOKEN_CHARACTERS};
use std::net::SocketAddr;
use std::time::Instant;

/// The most rooms an owner's list may hold.
const MAX_OWNED_ROOMS: usize = 100;
/// The lobby as it is first created. Later changes by its owner are kept.
pub const ROOM_REQUEST: &str = concat!(
    r#"{"id":"lobby","display_name":"SimplestChat Test Lobby","require_registration":false,"#,
    r#""max_participants":30,"max_broadcasters":6,"moderated":false,"secret":false,"#,
    r#""lobby_enabled":false,"guests_allowed":true,"guests_can_broadcast":true,"#,
    r#""topic":"Public test room. Test data may be reset; do not share sensitive information."}"#,
);
/// What a newly created lobby must say about itself.
const NEW_ROOM: [(&str, bool); 6] = [
    ("secret", false),
    ("passwordProtected", false),
    ("requireRegistration", false),
    ("lobbyEnabled", false),
    ("guestsAllowed", true),
    ("guestsCanBroadcast", true),
];

/// The account that owns the lobby.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub email: String,
    pub password: String,
}

/// What one seeding did. A failure keeps what was done before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub owner_created: bool,
    pub room_created: bool,
    pub existing_settings_preserved: bool,
    /// Whether the session was ended: `None` when none was ever held.
    pub refresh_session_revoked: Option<bool>,
    /// The fixed code of the first failure.
    pub failure: Option<&'static str>,
}

impl Report {
    /// Whether the lobby is in place and no session was left behind.
    pub fn passed(&self) -> bool {
        self.failure.is_none() && self.refresh_session_revoked == Some(true)
    }
}

/// A string as JSON text.
fn text(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// An account identity in its usual spelling: 32 hexadecimal digits in
/// groups of 8, 4, 4, 4 and 12.
fn identity(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let lengths = groups.iter().map(|group| group.len());
    let digits = |group: &&str| group.bytes().all(|byte| byte.is_ascii_hexdigit());
    lengths.eq([8, 4, 4, 4, 12]) && groups.iter().all(digits)
}

/// Whether `token` has the shape of an issued access token: 32 random bytes
/// as unpadded base64url. Nothing is read from it.
pub fn token_shape(token: &str) -> bool {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-';
    token.len() == TOKEN_CHARACTERS && token.bytes().all(plain)
}

/// The access token of an authentication answer for exactly this owner.
pub fn authentication<'a>(answer: Option<&'a Json>, email: &str) -> Result<&'a str, &'static str> {
    let answer = answer.ok_or("invalid_authentication_response")?;
    let user = answer
        .get("user")
        .filter(|user| matches!(user, Json::Object(_)));
    let user = user.ok_or("invalid_authentication_response")?;
    if user.get("email").and_then(Json::text) != Some(email) {
        return Err("owner_identity_mismatch");
    }
    let known = user.get("id").and_then(Json::text).is_some_and(identity);
    if !known {
        return Err("invalid_owner_identity");
    }
    let token = answer.get("token").and_then(Json::text);
    token
        .filter(|token| token_shape(token))
        .ok_or("invalid_authentication_token")
}

/// Whether a bounded, well-formed list of the owner's rooms holds the lobby,
/// which it may hold once.
pub fn owned_lobby(rooms: Option<&Json>) -> Result<bool, &'static str> {
    let Some(Json::Array(rooms)) = rooms else {
        return Err("invalid_owned_rooms");
    };
    if rooms.len() > MAX_OWNED_ROOMS {
        return Err("invalid_owned_rooms");
    }
    let mut lobbies = 0;
    for room in rooms {
        let name = room.get("id").and_then(Json::text);
        let name = name.ok_or("invalid_owned_rooms")?;
        lobbies += usize::from(name == "lobby");
    }
    match lobbies {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("ambiguous_owned_lobby"),
    }
}

/// Require a creation answer to be the open lobby that was asked for. It
/// names no owner; the owner's own list confirms that afterwards.
pub fn created_lobby(room: Option<&Json>) -> Result<(), &'static str> {
    let named = room.and_then(|room| room.get("id")).and_then(Json::text);
    if named != Some("lobby") {
        return Err("room_ownership_mismatch");
    }
    let room = room.ok_or("room_ownership_mismatch")?;
    let expected = |(field, value): &(&str, bool)| room.get(field) == Some(&Json::Bool(*value));
    match NEW_ROOM.iter().all(expected) {
        true => Ok(()),
        false => Err("unexpected_new_room_settings"),
    }
}

fn steps(api: &mut Api<'_>, owner: &Owner, report: &mut Report) -> Result<(), &'static str> {
    let (email, password) = (text(&owner.email), text(&owner.password));
    let credentials = format!(r#"{{"email":{email},"password":{password}}}"#);
    let (mut status, mut answer) = api.exchange(Operation::Login, Some(&credentials))?;
    if status == 401 {
        let registration = format!(
            r#"{{"email":{email},"password":{password},"display_name":"Test room owner"}}"#
        );
        (status, answer) = api.exchange(Operation::Register, Some(&registration))?;
        match status {
            200 => report.owner_created = true,
            409 => return Err("owner_credentials_conflict"),
            _ => return Err("owner_registration_failed"),
        }
    } else if status != 200 {
        return Err("owner_login_failed");
    }
    api.authenticated = true;
    let token = authentication(answer.as_ref(), &owner.email)?;
    api.token = Some(token.to_owned());
    if !api.has_cookie() {
        return Err("refresh_cookie_missing");
    }
    let (status, rooms) = api.exchange(Operation::Mine, None)?;
    if status != 200 {
        return Err("owned_rooms_unavailable");
    }
    if owned_lobby(rooms.as_ref())? {
        report.existing_settings_preserved = true;
        return Ok(());
    }
    let (status, room) = api.exchange(Operation::Create, Some(ROOM_REQUEST))?;
    if status != 200 {
        return Err("room_create_failed_or_conflict");
    }
    report.room_created = true;
    created_lobby(room.as_ref())?;
    let (status, rooms) = api.exchange(Operation::Mine, None)?;
    match status == 200 && owned_lobby(rooms.as_ref())? {
        true => Ok(()),
        false => Err("room_ownership_unconfirmed"),
    }
}

/// Seed the lobby for `owner` through the backend at `backend`, which
/// checks requests against `origin`. The session is always ended, within
/// its own time, and the report says whether it was.
pub fn seed(backend: SocketAddr, origin: &str, owner: &Owner, limits: &Limits) -> Report {
    let mut report = Report::default();
    let mut api = Api::new(backend, origin, limits);
    report.failure = steps(&mut api, owner, &mut report).err();
    api.until = Instant::now() + limits.farewell;
    match api.logout() {
        Ok(revoked) => report.refresh_session_revoked = revoked,
        Err(_) => {
            report
                .failure
                .get_or_insert("refresh_session_cleanup_failed");
            report.refresh_session_revoked = Some(false);
        }
    }
    report
}
