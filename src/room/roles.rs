#![forbid(unsafe_code)]

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Guest = 0,
    User = 1,
    Member = 2,
    Moderator = 3,
    Admin = 4,
    Owner = 5,
}

impl Role {
    pub fn from_db(value: i16) -> Self {
        match value {
            1 => Role::Member,
            2 => Role::Moderator,
            3 => Role::Admin,
            _ => Role::User,
        }
    }

    pub fn to_db(self) -> i16 {
        match self {
            Role::Member => 1,
            Role::Moderator => 2,
            Role::Admin => 3,
            _ => 0,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Role::Owner => "~",
            Role::Admin => "&",
            Role::Moderator => "@",
            Role::Member => "+",
            Role::User => "",
            Role::Guest => "?",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Moderator => "moderator",
            Role::Member => "member",
            Role::User => "user",
            Role::Guest => "guest",
        }
    }

    pub fn can_moderate(&self, target: Role) -> bool {
        *self >= Role::Moderator && *self > target
    }

    pub fn can_set_role(&self, target: Role, new_role: Role) -> bool {
        match new_role {
            Role::Member => *self >= Role::Moderator && *self > target,
            Role::Moderator => *self >= Role::Admin && *self > target,
            Role::Admin => *self >= Role::Owner,
            _ => false,
        }
    }

    pub fn can_broadcast(&self, moderated: bool) -> bool {
        if moderated {
            *self >= Role::Member
        } else {
            true
        }
    }

    pub fn can_chat(&self, moderated: bool) -> bool {
        if moderated {
            *self >= Role::Member
        } else {
            true
        }
    }

    pub fn can_change_settings(&self) -> bool {
        *self >= Role::Admin
    }

    pub fn can_admit_lobby(&self) -> bool {
        *self >= Role::Moderator
    }
}

pub async fn resolve_role(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<&Uuid>,
    owner_id: &Uuid,
    is_authenticated: bool,
) -> Role {
    if let Some(uid) = user_id {
        if uid == owner_id {
            return Role::Owner;
        }

        if let Ok(Some(row)) = sqlx::query_as::<_, (i16,)>(
            "SELECT role FROM room_roles WHERE room_id = $1 AND user_id = $2"
        )
        .bind(room_id)
        .bind(uid)
        .fetch_optional(pool)
        .await
        {
            return Role::from_db(row.0);
        }

        if is_authenticated {
            return Role::User;
        }
    }

    Role::Guest
}

pub async fn set_role(
    pool: &PgPool,
    room_id: &str,
    user_id: &Uuid,
    role: Role,
    granted_by: &Uuid,
) -> Result<(), sqlx::Error> {
    let role_val = role.to_db();
    if role_val == 0 {
        sqlx::query("DELETE FROM room_roles WHERE room_id = $1 AND user_id = $2")
            .bind(room_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO room_roles (room_id, user_id, role, granted_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (room_id, user_id) DO UPDATE SET role = $3, granted_by = $4"
        )
        .bind(room_id)
        .bind(user_id)
        .bind(role_val)
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
        assert!(!Role::Moderator.can_set_role(Role::User, Role::Moderator));
    }
}
