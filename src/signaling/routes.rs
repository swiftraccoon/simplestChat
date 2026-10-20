//! The HTTP and WebSocket routes, grouped by the address budget each group spends:
//! every API group carries its own [`AuthGuard`] so one kind of traffic cannot
//! starve another, and the whole tree sits behind the drain gate, the static
//! fallback, the security headers and request observation.

use super::*;
use axum::routing::{delete, patch, post, put};

/// A route group behind its own address budget and the request timeout.
fn budgeted(routes: Router<SignalingServer>, guard: &AuthGuard) -> Router<SignalingServer> {
    routes
        .layer(middleware::from_fn_with_state(
            guard.clone(),
            guarded_api_request,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            HTTP_REQUEST_TIMEOUT,
        ))
}

impl SignalingServer {
    pub(crate) fn router(self) -> Router {
        let auth_routes = budgeted(
            Router::new()
                .route("/register", post(crate::auth::routes::register))
                .route("/login", post(crate::auth::routes::login))
                .route("/refresh", post(crate::auth::routes::refresh))
                .route("/logout", post(crate::auth::routes::logout))
                .route("/sessions", get(crate::auth::sessions::list))
                .route(
                    "/sessions/others",
                    delete(crate::auth::sessions::revoke_others),
                )
                .route("/sessions/{id}", delete(crate::auth::sessions::revoke))
                .route(
                    "/invites",
                    get(crate::auth::invites::list).post(crate::auth::invites::create),
                )
                .route("/invites/{id}", delete(crate::auth::invites::revoke))
                .route("/password", post(crate::auth::account::change_password))
                .route("/ws-ticket", post(crate::auth::ws_tickets::issue))
                .route(
                    "/recovery/key",
                    post(crate::auth::account::create_recovery_key),
                )
                .route(
                    "/recovery/redeem",
                    post(crate::auth::account::redeem_recovery),
                )
                .route(
                    "/passkey/register/start",
                    post(crate::auth::routes::passkey_register_start),
                )
                .route(
                    "/passkey/register/finish",
                    post(crate::auth::routes::passkey_register_finish),
                )
                .route(
                    "/passkey/login/start",
                    post(crate::auth::routes::passkey_login_start),
                )
                .route(
                    "/passkey/login/finish",
                    post(crate::auth::routes::passkey_login_finish),
                )
                .route("/passkeys", get(crate::auth::passkeys::list))
                .route("/passkeys/start", post(crate::auth::passkeys::start))
                .route(
                    "/passkeys/authorize",
                    post(crate::auth::passkeys::authorize),
                )
                .route("/passkeys/enroll", post(crate::auth::passkeys::enroll))
                .layer(DefaultBodyLimit::max(16 * 1024))
                .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT)),
            &self.guards.auth,
        );

        // Account application traffic has a finite independent budget so inbox
        // updates and multiple tabs cannot consume sign-in/refresh capacity.
        let account_routes = budgeted(
            Router::new()
                .route(
                    "/profile",
                    get(crate::auth::account::get_profile)
                        .patch(crate::auth::account::update_profile)
                        .layer(DefaultBodyLimit::max(256 * 1024)),
                )
                .route(
                    "/preferences",
                    get(crate::auth::account::get_preferences)
                        .patch(crate::auth::account::patch_preferences)
                        .layer(DefaultBodyLimit::max(32 * 1024)),
                )
                .route(
                    "/attachments/{id}",
                    get(crate::attachments::download).delete(crate::attachments::delete_pending),
                )
                .route(
                    "/contacts",
                    get(crate::auth::discovery::list_contacts)
                        .post(crate::auth::discovery::create_contact),
                )
                .route(
                    "/contacts/{peer}",
                    put(crate::auth::discovery::accept_contact)
                        .delete(crate::auth::discovery::remove_contact),
                )
                .route("/saved-rooms", get(crate::auth::discovery::saved_rooms))
                .route("/saved-rooms/{id}", put(crate::auth::discovery::save_room))
                .route(
                    "/notification-preferences",
                    get(crate::auth::notifications::get_preferences)
                        .patch(crate::auth::notifications::patch_preferences)
                        .put(crate::auth::notifications::put_preferences),
                )
                .route(
                    "/notification-preferences/conversations/{peer}",
                    put(crate::auth::notifications::put_conversation),
                )
                .route("/inbox", get(crate::room::history::inbox))
                .route("/inbox/unread", get(crate::room::history::unread_summary))
                .route(
                    "/push",
                    get(crate::push::status)
                        .put(crate::push::subscribe)
                        .delete(crate::push::unsubscribe),
                )
                .route(
                    "/inbox/{peer}/messages",
                    get(crate::room::history::messages).post(crate::room::history::send_message),
                )
                .route("/inbox/{peer}/read", put(crate::room::history::read))
                .route(
                    "/inbox/{peer}/messages/{messageId}",
                    put(crate::room::history::edit_message),
                )
                .route(
                    "/inbox/{peer}/messages/{messageId}/reactions",
                    put(crate::room::history::set_private_reaction),
                )
                .layer(DefaultBodyLimit::max(16 * 1024))
                .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT)),
            &self.guards.account,
        );

        // Opening consumes an account-data admission only until headers. The
        // body has its own bounded lease and must outlive the HTTP timeout.
        let account_update_routes = Router::new()
            .route("/api/auth/updates", get(crate::auth::updates::stream))
            .layer(middleware::from_fn_with_state(
                self.guards.account.clone(),
                guarded_api_request,
            ));

        // A binary upload has its own total deadline while retaining the same
        // account guard and five-second idle-body limit. It must not be nested
        // under the ordinary auth router's fifteen-second timeout.
        let attachment_upload_routes = Router::new()
            .route("/api/auth/attachments", post(crate::attachments::upload))
            .layer(DefaultBodyLimit::max(5 * 1024 * 1024))
            .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT))
            .layer(middleware::from_fn_with_state(
                self.guards.account.clone(),
                guarded_api_request,
            ))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                HTTP_UPLOAD_REQUEST_TIMEOUT,
            ));

        let room_routes = budgeted(
            Router::new()
                .route("/", get(crate::room::api::list_rooms))
                .route("/", post(crate::room::api::create_room))
                .route("/mine", get(crate::room::api::owned_rooms))
                .route("/memberships", get(crate::room::invites::list_memberships))
                .route("/invites/preview", post(crate::room::invites::preview))
                .route("/invites/redeem", post(crate::room::invites::redeem))
                .route(
                    "/{id}/invites",
                    get(crate::room::invites::list).post(crate::room::invites::create),
                )
                .route(
                    "/{id}/invites/{invite_id}",
                    delete(crate::room::invites::revoke),
                )
                .route(
                    "/{id}/identity",
                    patch(crate::room::api::update_room_identity)
                        .layer(DefaultBodyLimit::max(256 * 1024)),
                )
                .route(
                    "/{id}/administration",
                    get(crate::room::administration::overview),
                )
                .route(
                    "/{id}/administration/settings",
                    patch(crate::room::administration::patch_settings),
                )
                .route(
                    "/{id}/administration/members/{target}",
                    put(crate::room::administration::set_role),
                )
                .route(
                    "/{id}/administration/bans/{ban}",
                    delete(crate::room::administration::remove_ban),
                )
                .route(
                    "/{id}/administration/reports/{report}",
                    put(crate::room::administration::resolve_report),
                )
                .route(
                    "/{id}/administration/moderation",
                    post(crate::room::administration::moderate),
                )
                .route(
                    "/{id}/administration/lobby/{participant}",
                    post(crate::room::administration::lobby_action),
                )
                .route(
                    "/{id}/administration/sanctions",
                    post(crate::room::administration::sanction),
                )
                .route(
                    "/{id}/administration/{section}",
                    get(crate::room::administration::list),
                )
                .route("/{id}", delete(crate::room::api::delete_room))
                .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT)),
            &self.guards.room_api,
        );

        // A directory page's thumbnails share the avatar budget: they must never
        // spend the room API allowance the listing itself needs.
        let room_image_routes = budgeted(
            Router::new().route("/{id}/image", get(crate::room::api::room_image)),
            &self.guards.profile,
        );

        let metrics = self.metrics.clone();
        let telemetry_routes = budgeted(
            Router::new()
                .route("/api/telemetry", post(telemetry::ingest))
                .layer(DefaultBodyLimit::max(8 * 1024))
                .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT)),
            &self.guards.telemetry,
        );
        // Beside the nested auth router, on its own address budget: a room's
        // avatars must never spend a viewer's sign-in and refresh allowance.
        let profile_routes = budgeted(
            Router::new().route("/{id}", get(crate::auth::account::public_profile)),
            &self.guards.profile,
        );
        let routes = Router::new()
            .merge(attachment_upload_routes)
            .merge(account_update_routes)
            .merge(telemetry_routes)
            .route("/ws", get(ws_handler))
            .route("/health", get(health_handler))
            .route("/api/capabilities", get(capabilities_handler))
            .route("/ready", get(readiness_handler))
            .route("/metrics", get(metrics_handler))
            .route("/diagnostics/media", get(media_diagnostics::handler))
            .nest("/api/auth/profiles", profile_routes)
            .nest("/api/auth", auth_routes.merge(account_routes))
            .nest("/api/rooms", room_routes.merge(room_image_routes))
            .layer(middleware::from_fn_with_state(
                self.room_manager.drain_signal(),
                drain_admission,
            ))
            .with_state(self);

        with_static_fallback_and_security(routes).layer(middleware::from_fn_with_state(
            metrics,
            observe_http_request,
        ))
    }
}
