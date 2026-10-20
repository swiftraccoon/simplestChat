# Rust server

The server combines HTTP/authentication, WebSocket signaling, room state and
in-process native mediasoup workers. PostgreSQL is optional for guest media,
required for accounts and persistent community data.

## Architecture

```text
HTTP / WebSocket
       │
       ├── auth + room APIs ── PostgreSQL
       │
       └── signaling ── RoomManager ── MediaServer
                                         ├── worker pool
                                         ├── router per room
                                         └── participant transports / producers / consumers
```

| Area | Entry points |
| --- | --- |
| Configuration, startup and database pool | `config.rs`, `app.rs`, `db.rs` |
| Identifiers (room slug, participant, message and account UUIDs) | `ids.rs` |
| Admission drain and bounded shutdown | `shutdown.rs`, `app.rs` |
| Routes, origin/limits, WebSocket lifecycle | `signaling/mod.rs` (server state, guards and handlers; tests in `signaling/security_tests.rs`), `signaling/routes.rs` (the route tree and its per-group budgets), `signaling/connection.rs` (`connection_token_bucket.rs` holds its per-connection rate limits; tests in `connection_security_tests.rs` and the other `connection_*_tests.rs` files) |
| Dependency readiness | `signaling/readiness.rs`, `media/worker_manager.rs` |
| Wire contract | `signaling/protocol.rs`; `cargo run -q -p xtask -- protocol-ts` generates [web/src/protocol.generated.ts](../web/src/protocol.generated.ts) from it |
| Password/passkey auth and JWT validation | `auth/routes.rs`, `auth/password.rs`, `auth/webauthn.rs`, `auth/jwt.rs`, `auth/limiter.rs` (sign-in failure delays, registration window) |
| Profiles, recovery and refresh sessions | `auth/account.rs`, `auth/session.rs` |
| Atomic account privacy and fresh runtime publication | `auth/account.rs`, `room/privacy.rs` |
| One-use WebSocket authentication tickets | `auth/ws_tickets.rs` |
| Membership, lobby, chat and reconnect state | `room/mod.rs` (room and manager state, admission tables, shared helpers; tests in `room/security_tests.rs`), `room/manager.rs` with `room/manager/{lifecycle,membership,media_ops,settings_ops,moderation_ops,lobby,counts}.rs` (the manager's methods by concern), `room/social.rs` (chat delivery, the request dispatcher and its `SocialRequest` lock hand-off; tests in `room/social_tests.rs`) with `room/social/{chat_history,preferences,members,reports}.rs` (one handler per social action), `room/window.rs` (per-room admission budgets) |
| Address cohort shared by every per-address limit and sanction | `net.rs` |
| Durable room history, PM inbox, read positions and chat expiry | `room/history.rs` |
| Account notification rules, quiet hours and per-conversation overrides | `auth/notifications.rs` |
| Bounded authenticated account invalidations outside rooms | `auth/updates.rs`, `auth/updates_tests.rs` |
| Bounded file storage, message ownership and short-lived live-room download grants | `attachments.rs` |
| Optional session-owned PM push, retained VAPID identity and bounded delivery queue | `push.rs` |
| Ordered persistence and uncertain-write handling | `room/control.rs`; `room/refusal.rs` (why an operation was refused: the class every persistence result carries, mapped once to the socket's error code and to an HTTP status) |
| Persistent room/community API | `room/api.rs`, `room/community.rs`, `room/settings.rs` |
| Membership-independent room administration and authorization | `room/administration.rs` |
| Roles, invitations and moderation retention | `room/roles.rs`, `room/invites.rs`, `room/moderation.rs` |
| Native worker/router/transport lifecycle | `media/worker_manager.rs`, `router_manager.rs`, `transport_manager.rs` |
| Media configuration and types | `media/config.rs`, `media/types.rs` |
| Prometheus metrics and TURN credentials | `metrics.rs`, `turn.rs` |
| Opt-in local operation traces | `diagnostics.rs`; [capture and interpretation](../docs/diagnostics.md) |

A module's unit tests live beside it as `<name>_tests.rs` once they outgrow a screen,
declared from the module as `#[cfg(test)] #[path = "<name>_tests.rs"] mod tests;` so
`use super::*` still reaches its private items (`room/history_tests.rs`,
`media/transport_manager_tests.rs`, `auth/account_tests.rs`, `config_tests.rs`, …).

Worker count is configurable (detected CPUs capped at 64 by default), not fixed
at 16. Each worker has a dedicated WebRtcServer UDP port; each room has one
primary router that holds every producer and the audio observers. New routers
select an open worker with an open listener, preferring fewer consumers and then
fewer pending or registered routers. Selection reserves the router count
atomically; cancellation, failed creation, and room removal release it
synchronously. This spreads rooms even before their users start media. Once a
room's primary worker carries 64 consumers, new receive transports are placed by
the same selection, and a room lazily gains one viewer router per other worker,
fed by router-to-router pipes created once per producer and worker; pipes follow
the producer's close, viewer routers close with the room, and a placement drops
with the participant's media. Consumers are created paused and resume after the
browser creates its consumer.
See [configuration](../docs/configuration.md) for allocation limits.

Authorization is server-side. Roles, bans, password access, lobby state and
media/chat permissions cannot rely on what the UI allows. Persistent mutations
must remain consistent with in-memory state; failed or stale async work must not
apply to a replacement room or membership.
Room control operations serialize policy writes and membership changes separately
from chat/media state. The lock order is creation (when needed), control, then
the short-lived room state lock. Never wait for SQL or acquire control while
holding room state. An admitted persistent mutation owns its task through runtime
publication, even if its caller disconnects. See [failure behavior](../docs/configuration.md#room-persistence).
Recovery checks the affected worker identity again after worker recreation. A
retired room reserves its ID until its previous router and participant media
finish cleanup. A new join cannot reuse that ID while old cleanup might still
remove its router; failed router teardown retains the reservation for process
recovery. Snapshot requests capture an owned, typed projection while holding a
shared room read lock and serialize it after releasing that lock. Private-message
visibility and moderator-only lobby entries are filtered during capture.

Authenticated HTTP administration lets an authorized account manage a persisted
room without joining it. Settings, roles, bans, reports, moderation history and
live lobby controls use the same room policy and runtime publication rules as
joined controls. Admission takes creation/identity coordination and room control
before the users-then-sessions transaction locks and the durable room row; it
rechecks current role and expiry after waits. Persistent mutations
retain ownership through commit and runtime publication; uncertain results remain
explicit rather than inviting an automatic replay. Camera/chat sanctions and
lobby admission still target the current live membership.

Account privacy PATCH operations merge explicit fields and ignore/unignore sets
with current JSON under the canonical users-then-sessions locks. Malformed saved
privacy fails closed. The owned save continues after HTTP disconnection and
publishes through `room/privacy.rs`: at most eight concurrent room operations,
with a fifteen-second overall synchronization deadline. Each room gate precedes
a fresh database read, so delayed earlier completions cannot overwrite newer
policy. Temporary guest ignores survive publication. Fresh authenticated joins
and lobby admissions load account policy under that gate before insertion. A
successful preference GET also verifies the issuing session and synchronizes
active rooms, providing explicit recovery after a save whose outcome is unknown.

`GET /api/auth/updates` carries content-free account invalidations over SSE,
authenticated by the ordinary Bearer header. A ready event and each reconnect
require fresh HTTP snapshots; events are hints, not durable state. Confirmed
mutations publish only after commit. Streams have per-account and global bounds,
close lagging receivers, revalidate sessions and retire during drain. See
[configuration](../docs/configuration.md#many-people-behind-one-address) for limits.
The hub is process-local: multiple server replicas require shared invalidations
before healthy streams can safely replace polling across replicas.

Saved history is separate from the 300-entry / 256-KiB membership replay buffer.
Migration 025 adds public-history retention to persisted rooms plus account PMs,
inbox rows and read cursors; migration 026 validates the expanded moderation
action constraint in a separate transaction. Public history defaults off; account PMs last 90
days. Retained public messages and account-to-account PMs use room control to
commit before delivery. Ephemeral text chat validates and publishes under the room
state lock without waiting for unrelated database writes. Ephemeral attachments
claim their uploads under the room control gate before delivery. A private message
involving a guest remains ephemeral. Inbox
HTTP routes authorize the current account and derive the two-account conversation
server-side; room history requires current membership. Pages and searches are
bounded. The separate five-minute chat cleanup removes expired messages, inbox
rows and cursors while read predicates enforce expiry immediately.

Account PM entries carry an explicit account-conversation marker so the browser
uses retained authority consistently from live and saved views. Saved replies
resolve the quoted message within that conversation; durable retries preserve
the quote target. Private reaction writes use explicit present/absent intent
under the conversation lock, with current account policy and session checks.
Retained public Reply/React resolve through saved history and current room
membership instead of requiring the target in the bounded replay buffer.

`auth/discovery.rs` stores explicitly accepted contact pairs and account room
shortcuts (migration 031). An accepted contact may start an HTTP PM without a
prior room conversation; private-message opt-out and ignore checks still apply.
Requests use an exact shared account identifier, with no email/name lookup,
100 active relationships per account and 30 new requests per day. Request and
accept operations lock users in UUID order, then recheck the current session.
Favorites and successful persisted-room admissions sync across account sessions;
100 favorites and 50 additional recents are retained. Unlisted room details are
filtered by current ownership/membership, not by remembered visits.

`removeChatMessage` requires Moderator+ and a public message belonging to that
room, including saved messages outside runtime replay. Its transaction removes
saved text and quoted excerpts alongside a `message_removed` moderation event.
Only message identity and actor/target metadata enter moderation history. Runtime
history and retry receipts are scrubbed before `chatMessageRemoved` is published;
removed messages reject new replies and reactions. PMs are excluded.

Author edits carry `expectedRevision`; accepted bodies increment `revision` and
set `editedAt`. Removal is terminal. Saved edit transactions update quote excerpts
and retain message identity; per-conversation advisory locks also serialize quoted
sends. Runtime edits update replay, receipts and pins, recalculate byte budgets,
and reject stale completion order. HTTP PM edits reconcile room state under room
control after their database commit. Cross-room publication releases the original
room gate before entering another, so simultaneous room edits cannot deadlock.

`getPinnedMessages`/`setPinnedMessage` expose at most three public pins; writes
require Moderator+. Migration 027 stores pins with cascading message deletion;
without saved room history they remain runtime-only. Retained pin reads enforce
message expiry, and removal/unpin/history-off clears them. Migrations 028–029 add
recipient-unread and conversation-quote indexes. Saved history supports bounded
`around` context, forward `after` pagination and first-unread `resume` positioning;
none of those reads advance an account's read marker. The inbox unread summary is
capped at 1,000 and never returns message contents.

Essential outbound queue overflow retires the affected WebSocket through
`signaling/outbound.rs`, independently of the full message queue. Registrations
hold weak sender references and leave with the connection handler. The reconnect
path supplies the snapshot needed after a missed event; ephemeral media hints
remain lossy. Normal successful fan-out does not consult the overflow registry.

Identity comparison lives in `labels.rs`: ICU Unicode data supplies full case
folding, compatibility normalization and default-ignorable handling. Displayed
spelling remains separate from the comparison key. Every account and guest join
receives an unambiguous room-local label, but full server-issued UUIDs remain the
message, authorization and moderation identity. Cross-script lookalikes are not
fully resolved by normalization; visible UUID discriminators provide the
additional identity cue. Room creation and identity updates share label checks.
Routine join/lobby logs contain IDs, not display names. User/participant IDs and
room IDs intentionally remain in lifecycle and moderation logs to correlate
failures and administrative actions; they are pseudonymous, potentially linkable
operational data, not anonymous telemetry. Structured moderation history retains
its separately governed labels, reasons and retention controls.

Account password selection and verification consistently use NFC. New selections
require 15–128 characters and use the same offline refusal list at registration,
change and recovery. The [pinned SecLists snapshot](../vendor/seclists-passwords/README.md)
and local curated entries reject known complete passwords and weak stems padded
with at most 16 ASCII digits/punctuation at each end. This bounded selection check
does not split passphrases or contact an external service. Source/license hashes
are pinned in regression tests, and container images carry the MIT notice. The
corpus is approximately 10,000 common credentials, not a complete breach database.
Standard Argon2id PHC hashes contain no compatibility marker.
Existing ASCII credentials naturally verify; a credential previously selected
with a non-NFC spelling may require account recovery. Password work remains
bounded outside the async executor. Password and passkey signup share the email
lookup budget. Failure delays are account-plus-address scoped, so a stream of
failures at other addresses cannot continuously deny the owner's proof. The HTTP, registration and
failure tables use bounded LRU eviction, with no shared overflow penalty;
resource admission remains a separate protection against distributed traffic.

Access JWTs require an explicit account `auth_version` and a UUID `sid` naming
an unexpired session belonging to that account. Every HTTP authorization and
socket revalidation enforces both; logout removes the session, and recovery or
password changes remove all sessions and advance the account version. There is
no sessionless token issuer or validator. Refresh tokens have one opaque format:
`v1n` followed by two 43-character unpadded base64url secrets (generation, then
family), each encoding 32 random bytes. Only hashes persist, the family hash is
required, and token rotation retains the existing bounded concurrency/replay rules.
Only the secure HttpOnly `__Host-refresh_token` cookie is issued and cleared.
Migration 022 invalidates all existing sign-ins once and makes the family hash
nonnullable; accounts, password/passkey credentials, recovery keys and memberships
are preserved. The startup schema check refuses a nullable family column.

`auth/sessions.rs` exposes account-owned session controls: `GET /api/auth/sessions`
lists up to 32 live sessions with sign-in, last-refresh and expiry timestamps and
a current-session marker. `DELETE /api/auth/sessions/{id}` revokes an individual
session; `DELETE /api/auth/sessions/others` keeps the caller's session and revokes
the rest. Neither listing nor revocation exposes refresh credentials or hashes.
Mutations recheck the caller under users-then-sessions locks, so a revoked caller
cannot complete a later revocation. Existing HTTP validation rejects the retired
session immediately; active sockets and retained reconnect media check every five
seconds. Revoking the current session clears its matching refresh cookie without
clearing a newer sign-in cookie installed by another tab.

Authenticated HTTP mints a 256-bit one-use upgrade ticket, consumed from the
`ticket.` WebSocket subprotocol. Pending records contain a digest, session-bound
claims and a monotonic deadline of at most 30 seconds; at most 10,000 records are
retained per process. Full capacity refuses issuance after reclaiming expired
records. Consumption is atomic before database revalidation; logout, credential
revocation and original access-token expiry are checked again before upgrade.
Existing connection expiry/renewal/revocation handling then owns the session.
Unknown protocols and reusable-JWT protocols are rejected instead of becoming guests.
This removes reusable JWTs from handshake headers, but tickets remain secret
until consumed/expired. A future load balancer must route ticket issuance and
upgrade to the same application process or provide an equivalent shared atomic
store; current deployment is a single application process.

Invitation creation returns a random 160-bit secret only once. Listings expose
metadata and an unrelated UUID used for revocation. `invites.code_hash` stores a
SHA-256 digest, and `invite_redemptions.invite_hash` references that digest.
Migration 021 invalidates outstanding old invitations and their receipts while
preserving existing memberships; production applies it before starting this
binary. Earlier backups and WAL can still retain prior plaintext until their
normal retention expires. No hashing migration promises secure erasure.

Room invitation preview and acceptance take JSON bodies and require an account.
Preview does not grant membership or join; acceptance requires an explicit user
action and still does not join. A redemption receipt commits with the role grant
and usage decrement. Repeating an account/code pair cannot consume a second use
or restore a subsequently removed role. Every membership listing returns a
stable room-ID cursor page with `items` and `next_cursor`. Pages contain at most
100 rows; pass the cursor as `after` to retrieve the remainder.

The retention job runs after one minute and every six hours thereafter. Each
statement selects at most 1,000 eligible parent rows, skips locked rows and
commits independently; a sweep processes at most 16 batches within 30 seconds.
Earlier committed batches survive later failure or cancellation. Addresses,
moderation history and resolved reports follow the configured day limits; live
sanctions and open reports remain. Expired invitations and their cascading
receipts are removed seven days after expiry. Each invitation may have up to 100
receipts, so cascading deletion adds work beyond the parent-row batch count.
Sweep changes are logged at info and failures at warning level; a large backlog
may require several sweeps to drain.

The [protocol guide](../docs/protocol.md) documents the browser/server contract,
including request correlation, replay limits and schema changes.
HTTP profiles and directory entries serialize snake_case fields, while room
settings and WebSocket messages use camelCase. Browser decoders rely on these
shapes, including required nullable profile/directory fields and no-content
statuses. Update the endpoint types, browser action/response contracts and
serialization tests together; the guide lists the HTTP method/result mappings.

Profile-card appearance (`users.profile_style`) and room name/topic appearance
(`rooms.name_style`, `rooms.topic_style`) are independent from chat styling.
Migrations 023/024 add and validate bounded JSON objects using the shared palette
and three treatments. Profile updates require the account session; room identity
updates require the owner and broadcast committed styles to current participants.
These fields require the maintenance deployment path, preserving existing data.

## Build and run

From the repository root, with the static OpenSSL/Python environment configured
as in [development](../docs/development.md):

```sh
cargo build --locked --release --bin simplestChat
```

Run from the repository root so `web/dist` and development migrations resolve.
Production builds do not enable `load-test`; synthetic clients live separately
under [load_tests](../load_tests/README.md).

No `DATABASE_URL` means anonymous-only operation. Auth also requires a JWT secret
of at least 32 bytes. `RUN_MIGRATIONS` defaults false; production uses a separate
DDL-capable migration role. Registration and ad-hoc rooms are opt-in.
See [deployment](../docs/deployment.md) before opening a service to other users.

## Validation and dependency changes

```sh
build/check.sh --rust
cargo test --locked --workspace -- --test-threads=1
```

The quality check runs formatting, workspace Clippy with warnings rejected, the
over-length line ratchet (`cargo run -q -p xtask -- line-length`), the generated
TypeScript contract check (`cargo run -q -p xtask -- protocol-ts --check`) and
public/private Rustdoc with warnings denied. It selects the pinned
toolchain and static native environment for its own process; separate Cargo commands still
need the development shell setup. First-party compiler warnings and unfulfilled
lint expectations fail the build, and `unsafe` remains forbidden. See
[contribution standards](../CONTRIBUTING.md).

Generated API documentation is written to `target/doc` by the Rust quality gate.
Its link/HTML checks do not establish documentation completeness. Public lifecycle
methods should explain ownership, side effects, failures and retry/cancellation
semantics, especially when cloning native handles or persisting shared state.
Keep these contracts in Rustdoc near the implementation; use this guide for
architecture and [the protocol guide](../docs/protocol.md) for wire behavior.

Database tests are ignored by default. Use the disposable bootstrap and
`--include-ignored` procedure in [testing](../docs/testing.md); do not treat an
ordinary passing Cargo run as database coverage.

Native tests reserve available UDP ports; serial execution avoids most of the
small reservation-to-bind race and does not require stopping another server.
Keep the macOS release build-script/proc-macro override.

Native source patches and security checks are documented in
[vendor/README.md](../vendor/README.md). Cargo audit alone does not inspect bundled
C/C++ code. Preserve provenance, pinned OpenSSL and the native-version CI gates
when changing dependencies.

## Notification policy

Migration 032 stores global notification policy and at most 100 conversation
overrides per account. Quiet hours use a validated PostgreSQL time-zone name,
start inclusive/end exclusive, including midnight and daylight-saving changes.
Snoozes are limited to 30 days. Push eligibility rechecks current preferences,
per-peer overrides, unread state and existing PM consent before dispatch.

## Attachments

Migration 033 stores attachment bytes in PostgreSQL, inside the existing backup
and migration boundary. Limits are 5 MiB/file, four/message, 64 MiB or 1,000 files
per account and 1 GiB or 20,000 files globally. Four upload permits bound body
reads; four download permits stay owned until the streamed body ends or drops.
Quota accounting includes expired bytes until physical cleanup. Pending files
expire after an hour. Durable claims occur in the message transaction; message
expiry/removal deletes the associated bytes. Ephemeral files require the original
message to remain visible in a live room and expire after 24 hours. The existing
five-minute retention worker deletes expired uploads in batches of 1,000.

Only accounts upload. A current account token reads its own pending files or
retained PM attachments. Live-room viewers, including guests, request a 30-second
file-specific grant through their current socket. Each download rechecks live
membership, media-session generation, message visibility, and any account session
before returning bytes. No token is put in a URL. PNG/JPEG/WebP dimension checks
bound previews to 4096 pixels per side and 16 million pixels; other files are
served as downloads with nosniff and a sandbox CSP. Filenames are sanitized.
