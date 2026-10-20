//! Identifiers shared by the wire protocol, the room runtime and persistence.
//!
//! Each newtype validates once at the boundary (deserialization, parsing or minting) so the rest of
//! the server never re-checks a room slug or re-parses a UUID string. On the wire every identifier
//! is a JSON string, exactly as before.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The longest room identifier the server accepts.
pub const MAX_ROOM_ID_LEN: usize = 128;

/// A room identifier: 1 to 128 ASCII letters, digits, hyphens or underscores.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[serde(try_from = "String", into = "String")]
#[sqlx(transparent)]
pub struct RoomId(String);

/// Why a room identifier was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRoomId {
    #[error("room id is empty")]
    Empty,
    #[error("room id exceeds {MAX_ROOM_ID_LEN} bytes")]
    TooLong,
    #[error("room id may only use ASCII letters, digits, hyphens and underscores")]
    Character,
}

impl RoomId {
    /// Validate and wrap a room identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidRoomId> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidRoomId::Empty);
        }
        if value.len() > MAX_ROOM_ID_LEN {
            return Err(InvalidRoomId::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(InvalidRoomId::Character);
        }
        Ok(Self(value))
    }

    /// The identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RoomId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for RoomId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl FromStr for RoomId {
    type Err = InvalidRoomId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for RoomId {
    type Error = InvalidRoomId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RoomId> for String {
    fn from(value: RoomId) -> Self {
        value.0
    }
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        uuid_id!(@define $(#[$doc])* $name, #[derive(sqlx::Type)] #[sqlx(transparent)]);
    };
    ($(#[$doc:meta])* $name:ident, text) => {
        uuid_id!(@define $(#[$doc])* $name,);
    };
    (@define $(#[$doc:meta])* $name:ident, $(#[$attr:meta])*) => {
        $(#[$doc])*
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a fresh random identifier.
            pub fn random() -> Self {
                Self(Uuid::new_v4())
            }

            /// The underlying UUID.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(
    /// A participant: the account's subject for signed-in users, a fresh UUID for guests.
    ParticipantId,
    text
);

// Participant identifiers are stored as text (guests never have an account row), so
// the database sees the UUID's canonical string form.
impl sqlx::Type<sqlx::Postgres> for ParticipantId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for ParticipantId {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'_, sqlx::Postgres>>::encode(self.0.to_string(), buf)
    }
}

impl sqlx::Decode<'_, sqlx::Postgres> for ParticipantId {
    fn decode(value: sqlx::postgres::PgValueRef<'_>) -> Result<Self, sqlx::error::BoxDynError> {
        let text = <&str as sqlx::Decode<'_, sqlx::Postgres>>::decode(value)?;
        Ok(text.parse()?)
    }
}

uuid_id!(
    /// A chat message.
    MessageId
);
uuid_id!(
    /// A registered account.
    UserId
);

/// Deterministic identifiers for tests: names stay readable while the types stay real.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{MessageId, ParticipantId, RoomId};
    use mediasoup::prelude::{ConsumerId, ProducerId, TransportId};
    use uuid::Uuid;

    /// A stable UUID for a name: a literal UUID is itself, anything else is hashed
    /// (FNV-1a over two 64-bit lanes; no crate feature needed).
    fn named(name: &str) -> Uuid {
        if let Ok(literal) = Uuid::parse_str(name) {
            return literal;
        }
        let mut high: u64 = 0xcbf2_9ce4_8422_2325;
        let mut low: u64 = 0x84222325cbf29ce4;
        for byte in name.bytes() {
            high = (high ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            low = (low ^ u64::from(byte).rotate_left(3)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        Uuid::from_u64_pair(high, low)
    }

    /// A room identifier that must satisfy the production grammar.
    pub(crate) fn rid(id: &str) -> RoomId {
        RoomId::new(id).expect("test room identifier follows the grammar")
    }

    /// A participant identifier derived from a readable name.
    pub(crate) fn pid(name: &str) -> ParticipantId {
        ParticipantId::from(named(name))
    }

    /// A message identifier derived from a readable name.
    pub(crate) fn mid(name: &str) -> MessageId {
        MessageId::from(named(name))
    }

    /// A mediasoup identifier derived from a readable name (the types parse UUIDs).
    fn media_id<T: std::str::FromStr>(name: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        named(name)
            .to_string()
            .parse()
            .expect("a UUID parses as a mediasoup identifier")
    }

    /// A transport identifier derived from a readable name.
    pub(crate) fn trid(name: &str) -> TransportId {
        media_id(name)
    }

    /// A producer identifier derived from a readable name.
    pub(crate) fn prid(name: &str) -> ProducerId {
        media_id(name)
    }

    /// A consumer identifier derived from a readable name.
    pub(crate) fn coid(name: &str) -> ConsumerId {
        media_id(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_ids_follow_the_slug_grammar() {
        assert!(RoomId::new("lobby-1_A").is_ok());
        assert_eq!(RoomId::new(""), Err(InvalidRoomId::Empty));
        assert_eq!(RoomId::new("a".repeat(129)), Err(InvalidRoomId::TooLong));
        assert_eq!(
            RoomId::new("a".repeat(128)).map(|id| id.as_str().len()),
            Ok(128)
        );
        for bad in ["a b", "a/b", "ä", "a.b", "a\0"] {
            assert_eq!(RoomId::new(bad), Err(InvalidRoomId::Character), "{bad:?}");
        }
    }

    #[test]
    fn room_ids_validate_during_deserialization() {
        let id: RoomId = serde_json::from_str("\"lobby\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"lobby\"");
        assert!(serde_json::from_str::<RoomId>("\"bad room\"").is_err());
        assert!(serde_json::from_str::<RoomId>("\"\"").is_err());
    }

    #[test]
    fn uuid_identifiers_keep_their_string_form_on_the_wire() {
        let id = ParticipantId::random();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<ParticipantId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<MessageId>("\"not-a-uuid\"").is_err());
        assert_eq!(
            "00000000-0000-0000-0000-000000000000"
                .parse::<UserId>()
                .unwrap()
                .as_uuid(),
            Uuid::nil()
        );
    }
}
