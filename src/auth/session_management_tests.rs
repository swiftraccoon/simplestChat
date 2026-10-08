use super::*;
use crate::auth::jwt;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_session_management_preserves_ownership_current_session_and_rotated_ids() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let mut users = Vec::new();
    let mut sessions = Vec::new();
    let mut claims = Vec::new();
    for _ in 0..2 {
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users(email,display_name) VALUES($1,'Session test') RETURNING id",
        )
        .bind(format!("sessions-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        users.push(user);
        for _ in 0..3 {
            let refresh = session::generate_refresh_token().unwrap();
            let id = session::create_session(&pool, &user, &refresh)
                .await
                .unwrap();
            let token = jwt::create_session_token(
                &user.to_string(),
                "Session test",
                "session-management-test-secret-with-enough-bytes",
                0,
                id,
            )
            .unwrap();
            claims.push(
                jwt::validate_token(&token, "session-management-test-secret-with-enough-bytes")
                    .unwrap(),
            );
            sessions.push((id, refresh));
        }
    }
    let initial = list_sessions(&pool, &claims[0]).await.unwrap();
    assert_eq!(initial.len(), 3);
    assert_eq!(initial[0].id, sessions[0].0);
    assert_eq!(initial.iter().filter(|session| session.current).count(), 1);
    let serialized = serde_json::to_value(&initial).unwrap();
    for value in serialized.as_array().unwrap() {
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert!(value.get("refresh_token_hash").is_none());
    }
    let mut transaction = pool.begin().await.unwrap();
    let rotated = session::rotate_refresh_token_with(&mut transaction, &sessions[1].1.raw)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(
        matches!(rotated, session::RefreshRotation::Rotated { session_id, .. } if session_id == sessions[1].0)
    );
    let after = list_sessions(&pool, &claims[0]).await.unwrap();
    assert!(
        after
            .iter()
            .find(|value| value.id == sessions[1].0)
            .unwrap()
            .refreshed_at
            >= initial
                .iter()
                .find(|value| value.id == sessions[1].0)
                .unwrap()
                .refreshed_at
    );

    revoke_sessions(&pool, &claims[0], Some(sessions[3].0), None)
        .await
        .unwrap();
    jwt::validate_current_claims(&pool, &claims[3])
        .await
        .unwrap();
    revoke_sessions(&pool, &claims[0], Some(sessions[1].0), None)
        .await
        .unwrap();
    assert!(
        jwt::validate_current_claims(&pool, &claims[1])
            .await
            .is_err()
    );
    jwt::validate_current_claims(&pool, &claims[2])
        .await
        .unwrap();
    // Claims obtained before revocation cannot mutate a surviving session.
    assert!(matches!(
        revoke_sessions(&pool, &claims[1], Some(sessions[0].0), None).await,
        Err(AuthError::InvalidToken)
    ));
    revoke_sessions(&pool, &claims[0], None, None)
        .await
        .unwrap();
    jwt::validate_current_claims(&pool, &claims[0])
        .await
        .unwrap();
    assert!(
        jwt::validate_current_claims(&pool, &claims[2])
            .await
            .is_err()
    );
    assert_eq!(list_sessions(&pool, &claims[0]).await.unwrap().len(), 1);
    assert_eq!(list_sessions(&pool, &claims[3]).await.unwrap().len(), 3);
    assert!(
        !revoke_sessions(
            &pool,
            &claims[3],
            Some(sessions[3].0),
            Some(&sessions[4].1.raw)
        )
        .await
        .unwrap(),
        "a newer cookie must survive revocation of another bearer session"
    );
    jwt::validate_current_claims(&pool, &claims[4])
        .await
        .unwrap();
    assert!(
        revoke_sessions(
            &pool,
            &claims[0],
            Some(sessions[0].0),
            Some(&sessions[0].1.raw)
        )
        .await
        .unwrap(),
        "revoking the cookie-backed current session clears that cookie"
    );
    assert!(
        jwt::validate_current_claims(&pool, &claims[0])
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&users)
        .execute(&pool)
        .await
        .unwrap();
}
