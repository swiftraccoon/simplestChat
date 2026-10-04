//! Passive authorization contract tests. Requests use the real in-process Axum
//! router; the ignored cases require an explicitly disposable PostgreSQL cluster.

use super::*;
use crate::auth::{jwt, session};
use crate::room::roles::{self, Role};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tower::ServiceExt;
use uuid::Uuid;

#[path = "auth_secret_canary_tests.rs"]
mod secret_canary_tests;

#[path = "appearance_tests.rs"]
mod appearance_tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Manifest {
    schema_version: u8,
    pub(super) http: Vec<HttpOperation>,
    pub(super) websocket: Vec<SocketOperation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HttpOperation {
    method: String,
    path: String,
    handler: String,
    policy: String,
    body: Option<Value>,
    unauthenticated_status: Option<u16>,
    checks: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SocketOperation {
    pub(super) variant: String,
    pub(super) operation: String,
    pub(super) boundary: String,
    pub(super) lobby: String,
    pub(super) body: Value,
    checks: Vec<String>,
}

pub(super) fn manifest() -> Manifest {
    let value: Manifest = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/security/authorization/operations.json"
    )))
    .unwrap();
    assert_eq!(value.schema_version, 1);
    value
}

#[test]
fn authorization_role_policy_matches_independent_expected_matrix() {
    use Role::*;
    let roles = [Guest, User, Member, Moderator, Admin, Owner];
    // Explicit product policy, indexed Guest through Owner. Avoid deriving the
    // oracle from Role ordering or repeating its comparison implementation.
    let permitted_targets: [&[Role]; 6] = [
        &[],
        &[],
        &[],
        &[Guest, User, Member],
        &[Guest, User, Member, Moderator],
        &[Guest, User, Member, Moderator, Admin],
    ];
    for (index, actor) in roles.into_iter().enumerate() {
        assert_eq!(
            actor.can_change_settings(),
            [false, false, false, false, true, true][index]
        );
        assert_eq!(
            actor.can_admit_lobby(),
            [false, false, false, true, true, true][index]
        );
        for moderated in [false, true] {
            let allowed = !moderated || [false, false, true, true, true, true][index];
            assert_eq!(actor.can_chat(moderated), allowed, "{actor:?} chat");
            assert_eq!(
                actor.can_broadcast(moderated),
                allowed,
                "{actor:?} broadcast"
            );
        }
        for target in roles {
            let allowed = permitted_targets[index].contains(&target);
            assert_eq!(
                actor.can_moderate(target),
                allowed,
                "{actor:?} -> {target:?}"
            );
            for grant in roles {
                assert_eq!(
                    actor.can_set_role(target, grant),
                    allowed && permitted_targets[index].contains(&grant),
                    "{actor:?} changes {target:?} to {grant:?}"
                );
            }
        }
    }
}

// This parser follows the real Router expression, including nested prefixes and
// chained method handlers. An unrecognised router construction fails instead of
// silently omitting a route from the policy inventory.
type Route = (String, String, String);
fn path(expression: &syn::Expr) -> String {
    let syn::Expr::Path(value) = expression else {
        panic!("expected handler path")
    };
    value
        .path
        .segments
        .iter()
        .map(|part| part.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}
fn literal(expression: &syn::Expr) -> String {
    let syn::Expr::Lit(value) = expression else {
        panic!("expected literal route")
    };
    let syn::Lit::Str(value) = &value.lit else {
        panic!("expected string route")
    };
    value.value()
}
fn methods(expression: &syn::Expr) -> Vec<(String, String)> {
    match expression {
        syn::Expr::Call(call) => {
            let method = path(&call.func);
            assert!(
                ["get", "post", "put", "patch", "delete", "head", "options"]
                    .contains(&method.as_str())
            );
            assert_eq!(call.args.len(), 1);
            vec![(method.to_ascii_uppercase(), path(&call.args[0]))]
        }
        syn::Expr::MethodCall(call) => {
            let mut rows = methods(&call.receiver);
            let method = call.method.to_string();
            if method != "layer" {
                assert!(
                    ["get", "post", "put", "patch", "delete", "head", "options"]
                        .contains(&method.as_str())
                );
                assert_eq!(call.args.len(), 1);
                rows.push((method.to_ascii_uppercase(), path(&call.args[0])));
            }
            rows
        }
        _ => panic!("unsupported method router"),
    }
}
fn routes(expression: &syn::Expr, bindings: &BTreeMap<String, Vec<Route>>) -> Vec<Route> {
    match expression {
        syn::Expr::Call(call) => {
            assert_eq!(path(&call.func), "Router::new");
            assert!(call.args.is_empty());
            Vec::new()
        }
        syn::Expr::Path(_) => bindings
            .get(&path(expression))
            .expect("known router binding")
            .clone(),
        syn::Expr::MethodCall(call) => {
            let mut rows = routes(&call.receiver, bindings);
            match call.method.to_string().as_str() {
                "route" => {
                    assert_eq!(call.args.len(), 2);
                    let route = literal(&call.args[0]);
                    rows.extend(
                        methods(&call.args[1])
                            .into_iter()
                            .map(|(method, handler)| (method, route.clone(), handler)),
                    );
                }
                "merge" => rows.extend(routes(&call.args[0], bindings)),
                "nest" => {
                    let prefix = literal(&call.args[0]);
                    rows.extend(routes(&call.args[1], bindings).into_iter().map(
                        |(method, route, handler)| {
                            let route = if route == "/" {
                                prefix.clone()
                            } else {
                                format!("{prefix}{route}")
                            };
                            (method, route, handler)
                        },
                    ));
                }
                "layer" | "with_state" => {}
                other => panic!("unreviewed Router method: {other}"),
            }
            rows
        }
        _ => panic!("unsupported router expression"),
    }
}

#[derive(Default)]
struct TestInventory(BTreeSet<String>);
impl<'ast> syn::visit::Visit<'ast> for TestInventory {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if item.attrs.iter().any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        }) {
            self.0.insert(item.sig.ident.to_string());
        }
        syn::visit::visit_item_fn(self, item);
    }
}
fn collect_tests(directory: &std::path::Path, inventory: &mut TestInventory) {
    use syn::visit::Visit;
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "test inventory never follows symlinks");
        if kind.is_dir() {
            collect_tests(&entry.path(), inventory);
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            let source = std::fs::read_to_string(entry.path()).unwrap();
            inventory.visit_file(&syn::parse_file(&source).unwrap());
        }
    }
}

#[test]
fn authorization_inventory_matches_actual_routes_handlers_and_message_variants() {
    let manifest = manifest();
    let mut tests = TestInventory::default();
    collect_tests(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut tests,
    );
    for checks in manifest
        .http
        .iter()
        .map(|row| &row.checks)
        .chain(manifest.websocket.iter().map(|row| &row.checks))
    {
        assert!(
            !checks.is_empty(),
            "every operation needs executable coverage"
        );
        for check in checks {
            assert!(
                tests.0.contains(check),
                "coverage test disappeared: {check}"
            );
        }
    }
    let source = syn::parse_file(include_str!("mod.rs")).unwrap();
    let router = source
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(item) => Some(item),
            _ => None,
        })
        .flat_map(|item| &item.items)
        .find_map(|item| match item {
            syn::ImplItem::Fn(item) if item.sig.ident == "router" => Some(item),
            _ => None,
        })
        .unwrap();
    let mut bindings = BTreeMap::new();
    for statement in &router.block.stmts {
        if let syn::Stmt::Local(local) = statement
            && let syn::Pat::Ident(binding) = &local.pat
            && (binding.ident.to_string().ends_with("routes"))
        {
            let value = routes(&local.init.as_ref().unwrap().expr, &bindings);
            assert!(bindings.insert(binding.ident.to_string(), value).is_none());
        }
    }
    let actual: BTreeSet<_> = bindings.remove("routes").unwrap().into_iter().collect();
    let expected: BTreeSet<_> = manifest
        .http
        .iter()
        .map(|row| (row.method.clone(), row.path.clone(), row.handler.clone()))
        .collect();
    assert_eq!(expected.len(), manifest.http.len(), "duplicate HTTP policy");
    assert_eq!(
        actual, expected,
        "every route/method must name its actual handler and policy"
    );
    for row in &manifest.http {
        assert_eq!(
            row.unauthenticated_status.is_some(),
            row.policy != "current-session" && row.policy != "guest-or-ticket"
        );
        assert!(
            [
                "current-session",
                "registration",
                "credentials",
                "refresh-cookie",
                "recovery-key",
                "registration-challenge",
                "public-challenge",
                "passkey-challenge",
                "public-profile",
                "public-directory",
                "anonymous-bounded",
                "guest-or-ticket",
                "public-health",
                "public-capabilities",
                "public-readiness",
                "metrics-bearer",
                "metrics-bearer-opt-in"
            ]
            .contains(&row.policy.as_str()),
            "unreviewed policy {}",
            row.policy
        );
    }
    let protocol = syn::parse_file(include_str!("protocol.rs")).unwrap();
    let variants: BTreeSet<_> = protocol
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "ClientMessage" => Some(
                item.variants
                    .iter()
                    .map(|value| value.ident.to_string())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap();
    let declared: BTreeSet<_> = manifest
        .websocket
        .iter()
        .map(|row| row.variant.clone())
        .collect();
    assert_eq!(declared.len(), manifest.websocket.len());
    assert_eq!(
        variants, declared,
        "new ClientMessage requires a reviewed boundary fixture"
    );
    for row in manifest.websocket {
        let decoded: protocol::ClientMessage = serde_json::from_value(row.body).unwrap();
        assert_eq!(socket_boundary(&decoded), row.boundary);
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["type"],
            row.operation
        );
    }
}

struct Fixture {
    server: SignalingServer,
    pool: PgPool,
    users: Vec<Uuid>,
    sessions: Vec<Uuid>,
    tokens: Vec<String>,
}
impl Fixture {
    async fn new(count: usize) -> Self {
        assert_eq!(
            std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
            Ok("1")
        );
        let pool =
            PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("disposable database"))
                .await
                .unwrap();
        sqlx::migrate::Migrator::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("migrations")
                .as_path(),
        )
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
        let metrics = ServerMetrics::new();
        let mut media = crate::media::config::MediaConfig::default();
        media.worker_config.num_workers = 1;
        media.webrtc_server_port_base = 0;
        let manager = Arc::new(
            RoomManager::new(media, metrics.clone(), Some(pool.clone()))
                .await
                .unwrap(),
        );
        let mut server = SignalingServer::new(manager, None, metrics, Some(pool.clone())).unwrap();
        server.jwt_secret = Some("disposable-authorization-policy-fixture-secret".into());
        server.registration_enabled = false;
        server.metrics_token = Some("disposable-policy-metrics-secret-at-least-32-bytes".into());
        server.webauthn = Some(Arc::new(
            webauthn_rs::prelude::WebauthnBuilder::new(
                "localhost",
                &url::Url::parse("https://localhost").unwrap(),
            )
            .unwrap()
            .build()
            .unwrap(),
        ));
        server.challenge_store = Some(Arc::new(ChallengeStore::new()));
        let mut result = Self {
            server,
            pool,
            users: Vec::new(),
            sessions: Vec::new(),
            tokens: Vec::new(),
        };
        for _ in 0..count {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Policy fixture')")
                .bind(id)
                .bind(format!("{id}@policy.invalid"))
                .execute(&result.pool)
                .await
                .unwrap();
            let sid = session::create_session(
                &result.pool,
                &id,
                &session::generate_refresh_token().unwrap(),
            )
            .await
            .unwrap();
            result.users.push(id);
            result.sessions.push(sid);
            result.tokens.push(result.token(id, sid, 0));
        }
        result
    }
    fn token(&self, user: Uuid, sid: Uuid, version: i64) -> String {
        jwt::create_session_token(
            &user.to_string(),
            "Policy fixture",
            self.server.jwt_secret().unwrap(),
            version,
            sid,
        )
        .unwrap()
    }
    async fn request(
        &self,
        operation: &HttpOperation,
        token: Option<&str>,
        sequence: u16,
    ) -> Response {
        let mut request = Request::builder().method(operation.method.as_str()).uri(
            operation
                .path
                .replace("{id}", "00000000-0000-4000-8000-000000000001")
                .replace("{invite_id}", "00000000-0000-4000-8000-000000000002"),
        );
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = if let Some(body) = &operation.body {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let mut request = request.body(body).unwrap();
        // Each table row is an independent request case, not a rate-limit test.
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((
                [192, 0, (sequence >> 8) as u8, sequence as u8],
                12345,
            ))));
        self.server.clone().router().oneshot(request).await.unwrap()
    }
    async fn finish(self) {
        self.server.begin_draining();
        sqlx::query("DELETE FROM invites WHERE created_by = ANY($1)")
            .bind(&self.users)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM rooms WHERE owner_id = ANY($1)")
            .bind(&self.users)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(&self.users)
            .execute(&self.pool)
            .await
            .unwrap();
        self.pool.close().await;
    }
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_every_authenticated_route_rejects_missing_or_noncurrent_sessions() {
    let fixture = Fixture::new(2).await;
    let foreign = fixture.token(fixture.users[0], fixture.sessions[1], 0);
    let missing = fixture.token(fixture.users[0], Uuid::new_v4(), 0);
    let stale_version = fixture.token(fixture.users[1], fixture.sessions[1], 1);
    sqlx::query(
        "UPDATE sessions SET expires_at = clock_timestamp() - interval '1 second' WHERE id=$1",
    )
    .bind(fixture.sessions[0])
    .execute(&fixture.pool)
    .await
    .unwrap();
    let mut sequence = 1;
    for row in manifest()
        .http
        .iter()
        .filter(|row| row.policy == "current-session")
    {
        for (label, token) in [
            ("missing", None),
            ("wrong-account session", Some(foreign.as_str())),
            ("absent session", Some(missing.as_str())),
            ("stale version", Some(stale_version.as_str())),
            ("expired session", Some(fixture.tokens[0].as_str())),
        ] {
            let response = fixture.request(row, token, sequence).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} {}: {label}",
                row.method,
                row.path
            );
            sequence += 1;
        }
    }
    let unchanged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ANY($1) AND display_name='Policy fixture' AND auth_version=0 AND password_hash IS NULL")
        .bind(&fixture.users).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(
        unchanged, 2,
        "denied account writes leave identity and credentials unchanged"
    );
    let created: i64 = sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM rooms WHERE owner_id=ANY($1)) + (SELECT COUNT(*) FROM invites WHERE created_by=ANY($1))")
        .bind(&fixture.users).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(
        created, 0,
        "denied requests cannot create rooms or invitations"
    );
    fixture.finish().await;
}

fn operation(method: &str, path: String, body: Option<Value>) -> HttpOperation {
    HttpOperation {
        method: method.into(),
        path,
        body,
        handler: String::new(),
        policy: "current-session".into(),
        unauthenticated_status: None,
        checks: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_room_roles_and_owner_mutations_do_not_cross_rooms_or_accounts() {
    let fixture = Fixture::new(5).await;
    let room = format!("policy-{}", Uuid::new_v4());
    let other_room = format!("policy-{}", Uuid::new_v4());
    for (id, owner) in [(&room, fixture.users[4]), (&other_room, fixture.users[0])] {
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'Policy room')")
            .bind(id)
            .bind(owner)
            .execute(&fixture.pool)
            .await
            .unwrap();
    }
    for (index, role) in [(1, Role::Member), (2, Role::Moderator), (3, Role::Admin)] {
        roles::set_role(
            &fixture.pool,
            &room,
            &fixture.users[index],
            role,
            &fixture.users[4],
        )
        .await
        .unwrap();
    }
    let expected = [
        Role::User,
        Role::Member,
        Role::Moderator,
        Role::Admin,
        Role::Owner,
    ];
    let mut sequence = 1;
    for (index, role) in expected.into_iter().enumerate() {
        assert_eq!(
            roles::resolve_role(
                &fixture.pool,
                &room,
                Some(&fixture.users[index]),
                &fixture.users[4],
                true
            )
            .await
            .unwrap(),
            role
        );
        // Even an administrator in this room is only an ordinary user in the
        // other room; owner identity is taken from that room's actual row.
        let elsewhere = if index == 0 { Role::Owner } else { Role::User };
        assert_eq!(
            roles::resolve_role(
                &fixture.pool,
                &other_room,
                Some(&fixture.users[index]),
                &fixture.users[0],
                true
            )
            .await
            .unwrap(),
            elsewhere
        );
        let list = operation("GET", format!("/api/rooms/{room}/invites"), None);
        let response = fixture
            .request(&list, Some(&fixture.tokens[index]), sequence)
            .await;
        sequence += 1;
        assert_eq!(
            response.status(),
            [
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
                StatusCode::OK,
                StatusCode::OK
            ][index],
            "list {role:?}"
        );
        for (grant, allowed) in [
            (2, [false, false, false, true, true]),
            (3, [false, false, false, true, true]),
            (4, [false, false, false, false, true]),
        ] {
            let create = operation("POST", list.path.clone(), Some(json!({"role":grant})));
            let response = fixture
                .request(&create, Some(&fixture.tokens[index]), sequence)
                .await;
            sequence += 1;
            let status = response.status();
            assert_eq!(
                status,
                if allowed[index] {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                },
                "{role:?} invite role {grant}"
            );
            if status == StatusCode::OK {
                let data: Value = serde_json::from_slice(
                    &axum::body::to_bytes(response.into_body(), 16 * 1024)
                        .await
                        .unwrap(),
                )
                .unwrap();
                let revoke = operation(
                    "DELETE",
                    format!("{}/{}", list.path, data["id"].as_str().unwrap()),
                    None,
                );
                assert_eq!(
                    fixture
                        .request(&revoke, Some(&fixture.tokens[index]), sequence)
                        .await
                        .status(),
                    StatusCode::NO_CONTENT
                );
                sequence += 1;
            }
            assert!(
                crate::room::invites::list_room_invites(&fixture.pool, &room)
                    .await
                    .unwrap()
                    .is_empty(),
                "denied create cannot leave an invitation; allowed create was revoked"
            );
        }
        let identity = operation(
            "PATCH",
            format!("/api/rooms/{room}/identity"),
            Some(
                json!({"display_name":"Owner-selected label", "description":"", "image_url":null, "topic":null,
                    "name_style":{"color":"teal","style":"text"}, "topic_style":{"color":"violet","style":"bubble"}}),
            ),
        );
        let response = fixture
            .request(&identity, Some(&fixture.tokens[index]), sequence)
            .await;
        sequence += 1;
        assert_eq!(
            response.status(),
            if index == 4 {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            "identity {role:?}"
        );
        let saved: String = sqlx::query_scalar("SELECT display_name FROM rooms WHERE id=$1")
            .bind(&room)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(
            saved,
            if index == 4 {
                "Owner-selected label"
            } else {
                "Policy room"
            }
        );
    }
    // A real invitation ID from a different tenant does not authorize deletion,
    // even when the caller owns the URL's room and can manage its invitations.
    let invite = crate::room::invites::create_room_invite(
        &fixture.pool,
        &other_room,
        Role::Member,
        1,
        1,
        fixture.users[0],
    )
    .await
    .unwrap()
    .unwrap();
    let revoke = operation(
        "DELETE",
        format!("/api/rooms/{room}/invites/{}", invite.details.id),
        None,
    );
    assert_eq!(
        fixture
            .request(&revoke, Some(&fixture.tokens[4]), sequence)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    sequence += 1;
    assert_eq!(
        crate::room::invites::list_room_invites(&fixture.pool, &other_room)
            .await
            .unwrap()
            .len(),
        1
    );
    let create = operation("POST", "/api/auth/invites".into(), None);
    let response = fixture
        .request(&create, Some(&fixture.tokens[0]), sequence)
        .await;
    sequence += 1;
    assert_eq!(response.status(), StatusCode::OK);
    let data: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let revoke = operation(
        "DELETE",
        format!("/api/auth/invites/{}", data["id"].as_str().unwrap()),
        None,
    );
    assert_eq!(
        fixture
            .request(&revoke, Some(&fixture.tokens[4]), sequence)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    sequence += 1;
    assert_eq!(
        fixture
            .request(&revoke, Some(&fixture.tokens[0]), sequence)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    // The successful own-session profile read proves the fixture does not deny
    // every credential; the handler returns the authenticated account, not a
    // client-supplied user identifier.
    let profile = operation("GET", "/api/auth/profile".into(), None);
    let response = fixture
        .request(&profile, Some(&fixture.tokens[3]), sequence + 1)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let data: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(data["id"], fixture.users[3].to_string());
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_public_and_credential_routes_keep_their_declared_boundary() {
    let fixture = Fixture::new(1).await;
    for (index, row) in manifest().http.iter().enumerate() {
        if let Some(expected) = row.unauthenticated_status {
            let response = fixture
                .request(row, None, u16::try_from(index + 1).unwrap())
                .await;
            assert_eq!(
                response.status().as_u16(),
                expected,
                "{} {} ({})",
                row.method,
                row.path,
                row.policy
            );
            if row.policy == "public-challenge" {
                let data: Value = serde_json::from_slice(
                    &axum::body::to_bytes(response.into_body(), 32 * 1024)
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert!(data["ceremony_id"].is_string());
                assert!(
                    data["publicKey"]["allowCredentials"]
                        .as_array()
                        .is_none_or(Vec::is_empty),
                    "anonymous discovery cannot disclose account credential IDs"
                );
            }
        }
    }
    let profile = operation(
        "GET",
        format!("/api/auth/profiles/{}", fixture.users[0]),
        None,
    );
    let response = fixture.request(&profile, None, 100).await;
    assert_eq!(response.status(), StatusCode::OK);
    let data: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(data["id"], fixture.users[0].to_string());
    for field in [
        "email",
        "password_hash",
        "recovery_key_hash",
        "auth_version",
    ] {
        assert!(data.get(field).is_none(), "public profile exposed {field}");
    }
    fixture.finish().await;
}

fn socket_boundary(message: &protocol::ClientMessage) -> &'static str {
    use protocol::ClientMessage::*;
    match message {
        RenewAuthentication { .. } => "current-session-renewal",
        JoinRoom { .. } => "room-admission",
        LeaveRoom => "own-membership",
        GetRouterRtpCapabilities => "room-membership",
        CreateSendTransport => "room-membership",
        CreateRecvTransport => "room-membership",
        ConnectTransport { .. } => "room-membership",
        Produce { .. } => "room-membership",
        Consume { .. } => "room-membership",
        ResumeConsumer { .. } => "room-membership",
        PauseConsumer { .. } => "room-membership",
        CloseConsumer { .. } => "room-membership",
        CloseProducer { .. } => "room-membership",
        PauseProducer { .. } => "room-membership",
        ResumeProducer { .. } => "room-membership",
        Reconnect { .. } => "reconnect-binding",
        RestartIce { .. } => "room-membership",
        SetConsumerPreferredLayers { .. } => "room-membership",
        ChatMessage { .. } => "room-membership",
        PrivateMessage { .. } => "room-membership",
        RetryChatMessage(..) => "room-membership",
        SetChatPreferences { .. } => "room-membership",
        SetChatStyle { .. } => "room-membership",
        ReactToMessage { .. } => "room-membership",
        ChangeNickname { .. } => "room-membership",
        GetRoomSnapshot { .. } => "room-membership",
        ListRoomBans { .. } => "room-membership",
        RemoveRoomBan { .. } => "room-membership",
        ListRoomMembers { .. } => "room-membership",
        SetMemberRole { .. } => "room-membership",
        ReportParticipant { .. } => "room-membership",
        ListRoomReports { .. } => "room-membership",
        ResolveRoomReport { .. } => "room-membership",
        ListModerationEvents { .. } => "room-membership",
        Typing { .. } => "room-membership",
        CloseCam { .. } => "room-membership",
        CamBan { .. } => "room-membership",
        CamUnban { .. } => "room-membership",
        TextMute { .. } => "room-membership",
        TextUnmute { .. } => "room-membership",
        Kick { .. } => "room-membership",
        Ban { .. } => "room-membership",
        Unban { .. } => "room-membership",
        SetRole { .. } => "room-membership",
        RequestVoice => "room-membership",
        UpdateRoomSettings { .. } => "room-membership",
        SetTopic { .. } => "room-membership",
        AdmitFromLobby { .. } => "room-membership",
        DenyFromLobby { .. } => "room-membership",
    }
}
