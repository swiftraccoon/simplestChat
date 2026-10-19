#![forbid(unsafe_code)]

use sqlx::PgPool;
use uuid::Uuid;

/// A participant's standing in a room, ordered from least to most trusted.
///
/// One numbering serves every boundary: the wire carries the lowercase name,
/// `room_roles.role` and `invites.role` store the discriminant (only
/// `Member`..`Admin` are ever stored: `User` is the absence of a row and
/// `Owner` is `rooms.owner_id`), and `invite_redemptions.granted_role` records
/// what a redemption granted.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    sqlx::Type,
)]
#[serde(rename_all = "lowercase")]
#[repr(i16)]
pub enum Role {
    Guest = 0,
    User = 1,
    Member = 2,
    Moderator = 3,
    Admin = 4,
    Owner = 5,
}

impl Role {
    /// The wire and display name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Moderator => "moderator",
            Role::Member => "member",
            Role::User => "user",
            Role::Guest => "guest",
        }
    }

    /// Whether a saved membership row carries this role: `User` is the absence
    /// of a row and `Guest`/`Owner` are never stored.
    pub(crate) fn is_stored(self) -> bool {
        matches!(self, Role::Member | Role::Moderator | Role::Admin)
    }

    pub(crate) fn can_moderate(&self, target: Role) -> bool {
        *self >= Role::Moderator && *self > target
    }

    pub(crate) fn can_set_role(&self, target: Role, new_role: Role) -> bool {
        *self >= Role::Moderator && *self > target && *self > new_role && new_role != Role::Owner
    }

    pub(crate) fn can_broadcast(&self, moderated: bool) -> bool {
        if moderated {
            *self >= Role::Member
        } else {
            true
        }
    }

    pub(crate) fn can_chat(&self, moderated: bool) -> bool {
        if moderated {
            *self >= Role::Member
        } else {
            true
        }
    }

    pub(crate) fn can_change_settings(&self) -> bool {
        *self >= Role::Admin
    }

    pub(crate) fn can_admit_lobby(&self) -> bool {
        *self >= Role::Moderator
    }
}

pub(crate) async fn resolve_role(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<&Uuid>,
    owner_id: &Uuid,
    is_authenticated: bool,
) -> Result<Role, sqlx::Error> {
    if let Some(uid) = user_id {
        if uid == owner_id {
            return Ok(Role::Owner);
        }

        if let Some(role) = sqlx::query_scalar::<_, Role>(
            "SELECT role FROM room_roles WHERE room_id = $1 AND user_id = $2",
        )
        .bind(room_id)
        .bind(uid)
        .fetch_optional(pool)
        .await?
        {
            return Ok(role);
        }

        if is_authenticated {
            return Ok(Role::User);
        }
    }

    Ok(Role::Guest)
}

pub(crate) async fn set_role(
    pool: &PgPool,
    room_id: &str,
    user_id: &Uuid,
    role: Role,
    granted_by: &Uuid,
) -> Result<(), sqlx::Error> {
    if !role.is_stored() {
        sqlx::query("DELETE FROM room_roles WHERE room_id = $1 AND user_id = $2")
            .bind(room_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO room_roles (room_id, user_id, role, granted_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (room_id, user_id) DO UPDATE SET role = $3, granted_by = $4",
        )
        .bind(room_id)
        .bind(user_id)
        .bind(role)
        .bind(granted_by)
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_serialize_by_name_and_store_their_discriminant() {
        for (role, name, number) in [
            (Role::Guest, "guest", 0),
            (Role::User, "user", 1),
            (Role::Member, "member", 2),
            (Role::Moderator, "moderator", 3),
            (Role::Admin, "admin", 4),
            (Role::Owner, "owner", 5),
        ] {
            assert_eq!(serde_json::to_string(&role).unwrap(), format!("{name:?}"));
            assert_eq!(
                serde_json::from_str::<Role>(&format!("{name:?}")).unwrap(),
                role
            );
            assert_eq!(role.name(), name);
            assert_eq!(role as i16, number);
        }
        assert!(serde_json::from_str::<Role>("2").is_err());
        assert!(serde_json::from_str::<Role>("\"Owner\"").is_err());
        assert_eq!(
            [Role::Member, Role::Moderator, Role::Admin]
                .iter()
                .filter(|role| role.is_stored())
                .count(),
            3
        );
        assert!(!Role::User.is_stored() && !Role::Owner.is_stored() && !Role::Guest.is_stored());
    }

    #[test]
    fn test_role_ordering() {
        assert!(Role::Owner > Role::Admin);
        assert!(Role::Admin > Role::Moderator);
        assert!(Role::Moderator > Role::Member);
        assert!(Role::Member > Role::User);
        assert!(Role::User > Role::Guest);
    }

    #[test]
    fn test_can_moderate() {
        assert!(Role::Owner.can_moderate(Role::Admin));
        assert!(Role::Admin.can_moderate(Role::Moderator));
        assert!(Role::Moderator.can_moderate(Role::Member));
        assert!(Role::Moderator.can_moderate(Role::User));
        assert!(Role::Moderator.can_moderate(Role::Guest));
        assert!(!Role::Moderator.can_moderate(Role::Moderator));
        assert!(!Role::Moderator.can_moderate(Role::Admin));
        assert!(!Role::Member.can_moderate(Role::Guest));
    }

    #[test]
    fn test_can_broadcast() {
        assert!(Role::Owner.can_broadcast(true));
        assert!(Role::Member.can_broadcast(true));
        assert!(!Role::User.can_broadcast(true));
        assert!(Role::User.can_broadcast(false));
        assert!(Role::Guest.can_broadcast(false));
        assert!(!Role::Guest.can_broadcast(true));
    }

    #[test]
    fn test_can_set_role() {
        assert!(Role::Owner.can_set_role(Role::User, Role::Admin));
        assert!(Role::Admin.can_set_role(Role::User, Role::Moderator));
        assert!(!Role::Admin.can_set_role(Role::User, Role::Admin));
        assert!(Role::Moderator.can_set_role(Role::User, Role::Member));
        assert!(Role::Moderator.can_set_role(Role::Member, Role::User));
        assert!(Role::Admin.can_set_role(Role::Moderator, Role::User));
        assert!(Role::Owner.can_set_role(Role::Admin, Role::User));
        assert!(!Role::Moderator.can_set_role(Role::User, Role::Moderator));
        assert!(!Role::Admin.can_set_role(Role::Owner, Role::User));
        assert!(!Role::Owner.can_set_role(Role::Admin, Role::Owner));
    }
}
