//! Tests of `src/room/invites.rs`.

use super::*;
use crate::ids::fixtures::rid;

#[test]
fn issuer_rule_names_the_stored_role_values_it_compares() {
    assert_eq!(Role::Member as i16, 2);
    assert_eq!(Role::Moderator as i16, 3);
    assert_eq!(Role::Admin as i16, 4);
    assert!(issuer_still_authorized!().contains("rr.role >= 4 AND rr.role > i.role"));
}

#[test]
fn membership_query_accepts_only_the_optional_cursor() {
    for (uri, after) in [
        ("/api/rooms/memberships", None),
        ("/api/rooms/memberships?after=room-100", Some("room-100")),
    ] {
        let Query(params) = Query::<MembershipParams>::try_from_uri(&uri.parse().unwrap()).unwrap();
        assert_eq!(params.after.as_deref(), after);
    }
    for uri in [
        "/api/rooms/memberships?paginated=false",
        "/api/rooms/memberships?paginated=true",
        "/api/rooms/memberships?after=room-100&paginated=true",
        "/api/rooms/memberships?unknown=value",
    ] {
        assert!(Query::<MembershipParams>::try_from_uri(&uri.parse().unwrap()).is_err());
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_room_invites_grant_roles_once_each_without_demoting_anyone() {
    let pool =
        sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
    let owner = Uuid::new_v4();
    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    for (id, name) in [(owner, "Owner"), (alice, "Alice"), (bob, "Bob")] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(id)
            .bind(format!("{id}@invites.invalid"))
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    let room_id = rid(&format!("invites-{}", Uuid::new_v4()));
    sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'Invited room')")
        .bind(&room_id)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    roles::set_role(&pool, &room_id, &bob, Role::Moderator, &owner)
        .await
        .unwrap();

    let invite = create_room_invite(&pool, &room_id, Role::Member, 2, 7, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(invite_codes::is_valid(&invite.code));
    assert_eq!(
        (invite.details.role, invite.details.uses_left),
        (Role::Member, 2)
    );
    assert_eq!(list_room_invites(&pool, &room_id).await.unwrap().len(), 1);

    let listed = serde_json::to_value(list_room_invites(&pool, &room_id).await.unwrap()).unwrap();
    assert!(listed[0].get("code").is_none());
    assert!(listed[0].get("code_hash").is_none());
    assert_eq!(listed[0]["id"], invite.details.id.to_string());
    let stored: String = sqlx::query_scalar("SELECT code_hash FROM invites WHERE id = $1")
        .bind(invite.details.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, invite_codes::digest(&invite.code));
    assert_ne!(stored, invite.code);
    for _ in 0..2 {
        assert_eq!(
            preview_room_invite(&pool, &invite.code, alice)
                .await
                .unwrap(),
            Some((room_id.clone(), "Invited room".into(), Role::Member))
        );
    }
    assert_eq!(
        list_room_invites(&pool, &room_id).await.unwrap()[0].uses_left,
        2
    );
    assert_eq!(
        roles::resolve_role(&pool, &room_id, Some(&alice), &owner, true)
            .await
            .unwrap(),
        Role::User
    );

    // Alice becomes a member; a lost-response retry must not spend twice.
    let (redeemed_room, name, role) = redeem_room_invite(&pool, &invite.code, alice)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (redeemed_room.as_str(), name.as_str(), role),
        (room_id.as_str(), "Invited room", Role::Member)
    );
    assert_eq!(
        roles::resolve_role(&pool, &room_id, Some(&alice), &owner, true)
            .await
            .unwrap(),
        Role::Member
    );
    let repeated = redeem_room_invite(&pool, &invite.code, alice)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated,
        (room_id.clone(), "Invited room".into(), Role::Member)
    );
    assert_eq!(
        list_room_invites(&pool, &room_id).await.unwrap()[0].uses_left,
        1
    );
    // Bob is already a moderator: the code never demotes him.
    let (_, _, role) = redeem_room_invite(&pool, &invite.code, bob)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(role, Role::Moderator);
    assert!(
        redeem_room_invite(&pool, &invite.code, alice)
            .await
            .unwrap()
            .is_some(),
        "a spent code still replays its successful receipt"
    );
    assert!(list_room_invites(&pool, &room_id).await.unwrap().is_empty());

    // The owner stays the owner, an expired code is refused, revocation removes a live one.
    let expired = create_room_invite(&pool, &room_id, Role::Admin, 1, 1, owner)
        .await
        .unwrap()
        .unwrap();
    let (_, _, role) = redeem_room_invite(&pool, &expired.code, owner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(role, Role::Owner);
    let fresh = create_room_invite(&pool, &room_id, Role::Admin, 1, 1, owner)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE invites SET expires_at = now() - interval '1 minute' WHERE code_hash = $1")
        .bind(invite_codes::digest(&fresh.code))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        redeem_room_invite(&pool, &fresh.code, alice)
            .await
            .unwrap()
            .is_none()
    );
    let live = create_room_invite(&pool, &room_id, Role::Moderator, 5, 30, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(
        revoke_room_invite(&pool, &room_id, live.details.id)
            .await
            .unwrap()
    );
    assert!(
        !revoke_room_invite(&pool, &room_id, live.details.id)
            .await
            .unwrap()
    );

    // A code outlives its issuer's authority only on paper: Bob, promoted to
    // admin, mints one; demoted, his code hides from the list and preview and
    // refuses redemption; promoted again, it works; a credential reset deletes it.
    let carol = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Carol')")
        .bind(carol)
        .bind(format!("{carol}@invites.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    roles::set_role(&pool, &room_id, &bob, Role::Admin, &owner)
        .await
        .unwrap();
    let bobs = create_room_invite(&pool, &room_id, Role::Moderator, 3, 7, bob)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list_room_invites(&pool, &room_id).await.unwrap().len(), 1);
    roles::set_role(&pool, &room_id, &bob, Role::Moderator, &owner)
        .await
        .unwrap();
    assert!(list_room_invites(&pool, &room_id).await.unwrap().is_empty());
    assert!(
        preview_room_invite(&pool, &bobs.code, carol)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        redeem_room_invite(&pool, &bobs.code, carol)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        roles::resolve_role(&pool, &room_id, Some(&carol), &owner, true)
            .await
            .unwrap(),
        Role::User
    );
    roles::set_role(&pool, &room_id, &bob, Role::Admin, &owner)
        .await
        .unwrap();
    assert_eq!(
        list_room_invites(&pool, &room_id).await.unwrap()[0].uses_left,
        3
    );
    let (_, _, role) = redeem_room_invite(&pool, &bobs.code, carol)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(role, Role::Moderator);
    // Carol keeps her membership and her receipt; Bob's reset voids the code for others.
    assert_eq!(revoke_issued_invites(&pool, bob).await.unwrap(), 1);
    assert!(list_room_invites(&pool, &room_id).await.unwrap().is_empty());
    assert_eq!(
        roles::resolve_role(&pool, &room_id, Some(&carol), &owner, true)
            .await
            .unwrap(),
        Role::Moderator
    );
    let dave = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Dave')")
        .bind(dave)
        .bind(format!("{dave}@invites.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        redeem_room_invite(&pool, &bobs.code, dave)
            .await
            .unwrap()
            .is_none()
    );

    // Concurrent retries of a single-use code both observe one committed receipt.
    let retry = create_room_invite(&pool, &room_id, Role::Member, 1, 1, owner)
        .await
        .unwrap()
        .unwrap();
    let (first, second) = tokio::join!(
        redeem_room_invite(&pool, &retry.code, alice),
        redeem_room_invite(&pool, &retry.code, alice),
    );
    let first = first
        .unwrap()
        .expect("initial single-use redemption succeeds");
    assert_eq!(first.2, Role::Member);
    assert_eq!(Some(first), second.unwrap());
    let remaining: i32 = sqlx::query_scalar("SELECT uses_left FROM invites WHERE code_hash = $1")
        .bind(invite_codes::digest(&retry.code))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    // A role removed after redemption is never restored by a retry.
    sqlx::query("DELETE FROM room_roles WHERE room_id = $1 AND user_id = $2")
        .bind(&room_id)
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        redeem_room_invite(&pool, &retry.code, alice)
            .await
            .unwrap()
            .is_some()
    );
    let roles: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_roles WHERE room_id = $1 AND user_id = $2")
            .bind(&room_id)
            .bind(alice)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(roles, 0);
    // Receipt retries survive expiry for seven days; new users cannot redeem them.
    sqlx::query("UPDATE invites SET expires_at = now() - interval '1 day' WHERE code_hash = $1")
        .bind(invite_codes::digest(&retry.code))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        redeem_room_invite(&pool, &retry.code, alice)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        redeem_room_invite(&pool, &retry.code, bob)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE invites SET expires_at = now() - interval '8 days' WHERE code_hash = $1")
        .bind(invite_codes::digest(&retry.code))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        redeem_room_invite(&pool, &retry.code, alice)
            .await
            .unwrap()
            .is_none()
    );
    // Explicit revocation immediately invalidates an existing receipt too.
    assert!(
        revoke_room_invite(&pool, &room_id, invite.details.id)
            .await
            .unwrap()
    );
    assert!(
        redeem_room_invite(&pool, &invite.code, alice)
            .await
            .unwrap()
            .is_none()
    );
    let receipts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM invite_redemptions WHERE invite_hash = $1")
            .bind(invite_codes::digest(&invite.code))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipts, 0);
    sqlx::query("INSERT INTO room_roles(room_id,user_id,role,granted_by) VALUES($1,$2,$3,$4)")
        .bind(&room_id)
        .bind(alice)
        .bind(Role::Member)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();

    // The room holds twenty live codes at most.
    for _ in 0..MAX_LIVE_ROOM_INVITES {
        assert!(
            create_room_invite(&pool, &room_id, Role::Member, 1, 7, owner)
                .await
                .unwrap()
                .is_some()
        );
    }
    assert!(
        create_room_invite(&pool, &room_id, Role::Member, 1, 7, owner)
            .await
            .unwrap()
            .is_none()
    );

    // Memberships list the rooms an account belongs to, not the ones it owns.
    let mine = memberships(&pool, alice, None).await.unwrap();
    assert_eq!(mine.rows.len(), 1);
    assert!(mine.next_cursor.is_none());
    assert_eq!(
        (mine.rows[0].0.id.as_str(), mine.rows[0].1),
        (room_id.as_str(), Role::Member)
    );
    assert!(
        memberships(&pool, owner, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );

    sqlx::query("DELETE FROM rooms WHERE id=$1")
        .bind(&room_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner, alice, bob])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_memberships_paginate_every_room_and_ignore_display_name_changes() {
    let pool =
        sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
    let owner = Uuid::new_v4();
    let member = Uuid::new_v4();
    for id in [owner, member] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Paging account')")
            .bind(id)
            .bind(format!("{id}@membership.invalid"))
            .execute(&pool)
            .await
            .unwrap();
    }
    let prefix = format!("pages-{}-", Uuid::new_v4());
    let rooms: Vec<String> = (0..205)
        .map(|index| format!("{prefix}{index:03}"))
        .collect();
    sqlx::query("INSERT INTO rooms(id,owner_id,display_name) SELECT id,$2,'Same name' FROM unnest($1::text[]) AS id")
            .bind(&rooms).bind(owner).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO room_roles(room_id,user_id,role,granted_by) SELECT id,$2,$4,$3 FROM unnest($1::text[]) AS id")
            .bind(&rooms).bind(member).bind(owner).bind(Role::Member).execute(&pool).await.unwrap();
    let first = memberships(&pool, member, None).await.unwrap();
    assert_eq!(first.rows.len(), 100);
    assert_eq!(first.next_cursor.as_ref(), Some(&rooms[99]));
    sqlx::query("UPDATE rooms SET display_name='A new name',updated_at=now() WHERE id=ANY($1)")
        .bind(&rooms)
        .execute(&pool)
        .await
        .unwrap();
    let second = memberships(&pool, member, first.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(second.rows.len(), 100);
    assert_eq!(second.next_cursor.as_ref(), Some(&rooms[199]));
    let third = memberships(&pool, member, second.next_cursor.as_deref())
        .await
        .unwrap();
    assert_eq!(third.rows.len(), 5);
    assert!(third.next_cursor.is_none());
    let all: Vec<String> = first
        .rows
        .into_iter()
        .chain(second.rows)
        .chain(third.rows)
        .map(|(row, _)| row.id.to_string())
        .collect();
    assert_eq!(all, rooms);
    assert!(
        memberships(&pool, member, rooms.last().map(String::as_str))
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        memberships(&pool, owner, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    sqlx::query("DELETE FROM rooms WHERE id=ANY($1)")
        .bind(&rooms)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![owner, member])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}
