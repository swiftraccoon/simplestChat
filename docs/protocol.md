# Browser/server protocol

This guide describes the current browser/server contract, not a versioned public
API. Complete message fields live in [Rust's protocol](../src/signaling/protocol.rs)
and [TypeScript's protocol](../web/src/protocol.ts). Change both sides together.
HTTP account and room endpoints are separate from the WebSocket flow below.

## HTTP response contracts

Community screens use named methods on [the browser API client](../web/src/ui.ts).
Each method fixes its route, verb, request type and response decoder; callers do
not supply a result type. Requests stay same-origin, use a bearer token when
provided, and serialize a body as JSON only when the endpoint accepts one.
Login, passkeys and refresh remain owned by [AuthManager](../web/src/auth.ts).

| Browser method | HTTP route | Successful result |
| --- | --- | --- |
| `publicProfile` | `GET /api/auth/profiles/:id` | `PublicProfile` |
| `accountProfile` | `GET /api/auth/profile` | `AccountProfile` |
| `updateProfile` | `PATCH /api/auth/profile` | `AccountProfile` |
| `changePassword` | `POST /api/auth/password` | `204`, no body |
| `recoveryKey` | `POST /api/auth/recovery/key` | `{ recovery_key: string }` |
| `redeemRecovery` | `POST /api/auth/recovery/redeem` | `204`, no body |
| `rooms` | `GET /api/rooms` with directory query parameters | `RoomListItem[]` |
| `ownRooms` | `GET /api/rooms/mine` | `RoomListItem[]` |
| `createRoom` | `POST /api/rooms` | `RoomSettings` |
| `updateRoomIdentity` | `PATCH /api/rooms/:id/identity` | `RoomListItem` |
| `deleteRoom` | `DELETE /api/rooms/:id` | `204`, no body |

`RoomListItem.participant_count` and `broadcaster_count` are `null`, never zero,
when the room's live state was busy at listing time; the browser renders an
unknown count rather than an empty room.

Account/profile and directory fields use snake_case. `RoomSettings`, including
the room-creation result, uses camelCase. Profile `avatar_url`, directory `topic`
and directory `image_url` are required nullable fields; null is not a missing
response. Directory counts, description and visibility flags are also required.
Unknown response fields are stripped before use. See the
[HTTP decoders](../web/src/api-validation.ts), [account handlers](../src/auth/account.rs)
and [room handlers](../src/room/api.rs).

JSON endpoints reject empty or malformed successes. Only the three no-content
methods above accept `204`, and they do not attempt JSON parsing. `ApiError`
retains an unsuccessful HTTP status for UI decisions; invalid successful data
produces a fixed error without including the response body. A network failure
or invalid response does not prove a mutation was rolled back. Do not retry a
password change, recovery-key replacement, redemption or room creation blindly.

## Connection and authentication

The browser connects to `/ws` on the page's host, using `wss:` for HTTPS pages and
`ws:` for local HTTP. Messages are JSON text objects with a camelCase `type`
discriminant and camelCase fields.

The client offers the `simplestchat` subprotocol. An authenticated connection also
offers an `auth.`-prefixed JWT; the server selects only `simplestchat`, never the
credential-bearing value. Query-string tokens are rejected. A supplied invalid
token fails the upgrade instead of silently creating a guest connection. Without
a token, the connection is a guest. Keep JWTs and reconnect credentials out of
URLs, logs and saved examples.

Browser upgrades must satisfy the server's Origin policy. Use `ALLOWED_ORIGINS`
for explicit deployment origins; its unset development behavior permits matching
loopback origins. Origin-less native clients are handled separately. See
[configuration](configuration.md) for deployment settings.

Authentication comes from the handshake, not from participant IDs in messages.
The server also enforces JWT expiry and account revocation on open sockets.

The browser schedules HTTP token refresh after 12 minutes. Network failures or
HTTP 5xx responses retain the current session for up to three refresh requests
within 15 seconds of starting scheduled refresh, capped by the accepted token's
expiry. Retries wait three seconds plus up to 249 ms of jitter; waiting for the
same-origin refresh lock also consumes the deadline. Definitive rejection or a
malformed successful response clears the session. The existing replay-confirmation
delay for an exact `401` with `Invalid token` remains, and its request counts toward
the same limit. A lost response can still lead to replay rejection; retries do not
guarantee recovery of a rotated cookie. Initial session restoration does not gain
this transient-failure allowance.
New authentication actions retire pending refresh work but do not replenish the
old session's budget; budget expiry also retires a still-pending login or logout.

After an HTTP refresh, the browser sends `renewAuthentication` with a `requestId`
and the new `token` on its existing authenticated socket. `authenticationRenewed`
returns that request ID and `expiresAt` in Unix seconds; a rejected renewal returns
`authenticationRenewalFailed` with only the request ID. These connection-owned
replies are separate from room request/error correlation. Never log renewal frames.

Temporary database or authentication-capacity failures instead return
`authenticationRenewalDeferred`, containing the request ID, `retryAfterMs` and
the **unchanged accepted** `expiresAt`. This is not acceptance of the new token.
The browser keeps the socket and retries the latest pending token after the
requested delay plus up to 249 ms of jitter. It allows at most three total
attempts within one 15-second budget, shortened by the accepted expiry; repeated
deferrals and newer pending tokens cannot extend that budget.

Renewal validates the same account and credential version without changing room
membership, media, roles or reconnect credentials. It cannot upgrade a guest,
change identity, shorten the accepted lifetime or revive an expired connection.
The server bounds validation to two seconds and the old expiry, and permits three
renewal attempts per minute per socket under the shared authentication budget.
The browser retains the newest token for future handshakes. Terminal rejection,
a missing response after five seconds, or exhaustion of the deferred-retry budget
falls back to ordinary bounded reconnection.
Logout and identity changes still leave and replace the socket.

Open authenticated sockets revalidate account state every five seconds after the
previous check completes. A database error or validation timeout permits retaining
previously accepted credentials for at most 15 seconds from the first observed
failure, capped by the accepted JWT expiry. Only successful revalidation before
that deadline clears the allowance. Disconnecting carries the same deadline into
reconnect grace; it does not start a fresh allowance or extend the normal 30-second
grace period. Initial authenticated handshakes still require a successful database
check, so reconnect admission remains unavailable while the database is down.
Known revocation, missing authentication configuration and expired credentials
remain terminal. Lost revocation notifications require a fresh successful check;
uncertainty cannot substitute for that check.
Validation and receive waits are deadline-bounded; expiry does not roll back
already-dispatched database or media operations.

The server limits inbound frames/messages to 64 KiB and closes connections after
five minutes without an inbound frame. For quiet, currently bound room or lobby
members, it checks every 30 seconds whether a protocol Ping is needed. Browsers
reply with Pong automatically; native clients must continue polling their socket
to service control frames. There is no application `ping` message or browser
timer requirement. Sending Ping does not renew the idle deadline: only received
frames do. Unjoined, removed and nonresponsive clients still expire; heartbeat
does not extend credentials or reconnect grace. Rate limits and bounded outgoing
queues also apply. A connected WebSocket means neither room admission nor
working media.

When a peer sends a WebSocket Close frame, the server stops application writes
and allows up to one second to flush the protocol's close reply. For valid close
frames, the reply preserves the peer's code and reason; an empty Close remains
empty. This transport handshake is not a `leaveRoom` request and does not change
reconnect grace. It runs before any disconnect-time database credential check.

## Requests, replies and events

| Operation | Correlation and browser deadline |
| --- | --- |
| Media and reconnect `SignalingClient.request` | Generated `requestId` plus the command's expected response `type`; 5 seconds by default |
| Authentication renewal | Dedicated `requestId`; 5 seconds, one in flight per socket |
| Room join | Next `roomJoined`, `lobbyWaiting`, `roomPasswordRequired` or `error`; 10 seconds |
| Social action | Generated `requestId` plus matching `action`; 10 seconds, at most 32 pending |
| Chat send | `clientMessageId` reconciles the optimistic entry with an acknowledgement; 12 seconds before marking delivery unconfirmed |

An `error` carries a human-readable `message`. An error with a `requestId` rejects
only that pending request. Errors without IDs are connection or legacy command
notifications and cannot settle a pending media/reconnect request; they can reach
the join handler. `roomPasswordRequired` is a distinct retry/prompt result, while
`roomClosed` is terminal room state. `serverRestarting` is a separate temporary
process-shutdown event with a human-readable `reason`; it is not room deletion.
Do not infer machine-readable error codes from message text.

The browser assigns each acknowledged media/reconnect command a fresh `requestId`
and retains its sequence across room changes and reconnects. IDs contain 1–64
ASCII letters, digits, hyphens or underscores. The server echoes the ID on direct
success and error replies, including malformed-command and rate-limit errors
when the envelope contains a valid ID. Missing IDs remain supported for legacy
clients; present invalid IDs are rejected before dispatch. The browser requires
both the ID and the command's expected response type and never falls back to
matching only the type. Concurrent requests for the same response type can
therefore settle independently, in either order. Authentication and social
responses retain their dedicated correlation handlers.

Unknown, duplicate and expired correlated replies are discarded before event
dispatch. Deadlines use a monotonic clock and are checked on receipt as well as
by timeout callbacks. Broadcasts and follow-up notifications have no request ID;
for example, a paused producer's `producerPaused` event remains separate from
the correlated `consumerCreated` reply. The receive-setup queue still orders
media setup and UI updates. An ID correlates a reply; it does not cancel server
work, roll back a timed-out mutation or make a retry idempotent.

Unsolicited events update membership, moderation, chat or media state. Malformed
messages cannot resolve requests. Socket close/disconnect rejects pending
requests and clears their timers; callbacks from a replaced socket are ignored.
`send` does not queue messages while disconnected. Deploy the server support
with the browser: the new browser never matches requests to ID-less replies
from older servers.

The media manager separately retains the latest unsent controls for its existing
resources during signaling loss: producer/consumer pause or resume, closure,
layer selection and ICE restart. It applies these after the same room session
resumes and its snapshot is reconciled (or snapshot retrieval fails). Superseded
controls and resources revoked by the snapshot are discarded; leaving or doing
a fresh join discards the old manager's pending controls. Creation and capture
requests are never queued for replay. A second disconnect retires the previous
recovery attempt without letting it close or resume the new socket's media.

If signaling is lost during initial transport setup, the browser closes its
partial local media manager. Recovery first reclaims the admitted session, then
joins again so the server retires any transports whose creation replies were
lost. Rejoining creates transports without starting camera, microphone or screen
capture. Ordinary unsupported-browser setup errors remain chat-only; a rejected
initial admission is never treated as automatic rejoin intent. A second loss
during fresh admission cancels its waiter immediately, and late setup results
cannot report a closed or replaced socket as connected.

## Join, lobby and media

1. Send `joinRoom` with `roomId`, `participantName` and an optional password.
   The server determines authenticated identity, role and access. A successful
   `roomJoined` supplies the local participant ID, reconnect credential, existing
   participants/producers, role and optional persisted-room settings.
2. `lobbyWaiting` is not admission: do not initialize media while waiting.
   Moderators receive `lobbyJoin`. Admission sends `lobbyAdmitted` followed by
   `roomJoined`; denial sends `lobbyDenied`.
3. Once admitted, request `getRouterRtpCapabilities`, load the mediasoup client
   device, then request `createSendTransport` and `createRecvTransport` in order.
   `transportCreated` carries ICE/DTLS parameters and optional ICE-server entries.
4. Handle the client transport's connect callback with `connectTransport` and
   wait for `transportConnected`. This acknowledges native acceptance of the
   remote DTLS parameters, not completed ICE/DTLS negotiation or RTP delivery.
   Retries after successful acceptance are idempotent while that same transport
   remains usable; failed or closed transports are rejected. Early ICE/DTLS
   activity does not replace the first request.
   Publishing is separate: explicit user capture leads to `produce` /
   `producerCreated`, with `newProducer` notifying peers. Joining and transport
   creation do not enable the camera or microphone.
5. For an existing or new producer, send `consume` with receiver RTP capabilities.
   The server creates a **paused** consumer and returns `consumerCreated`. Create
   the browser consumer first, then send `resumeConsumer` and await
   `consumerResumed`. Neither acknowledgement establishes RTP delivery, decoded
   video, audible sound or permission to autoplay.
   If receiver creation or resumption fails, send `closeConsumer` with its
   `consumerId` to release the server subscription before retrying. This also
   releases a receiver that is no longer needed. It has no acknowledgement and
   repeated closure is harmless; only the current session's consumer is affected.

Producer pause/resume/close events describe shared publishing state. Consumer
pause/resume and preferred layers affect that receiver's subscription; local
playback volume is not producer moderation. `restartIce` / `iceRestarted` update
an existing transport and are distinct from reconnecting signaling; `iceRestarted`
carries fresh `iceServers` because the TURN credentials minted at transport
creation expire after `TURN_TTL`, and the browser installs them before it
regathers candidates.

`leaveRoom` has no dedicated acknowledgement. The browser immediately retires its
membership and media, and the server removes the corresponding membership.

## Chat, social actions and replay

Public `chatMessage` and `privateMessage` sends use a client-generated
`clientMessageId`. The sender receives `messageAck` containing the authoritative
`ChatEntry`; recipients receive `chatReceived` or `privateMessageReceived`.
Entries carry a server `messageId` and `sentAt`. A chat-related `socialError` can
carry `clientMessageId`; administrative social failures use `requestId`.

Social commands such as `getRoomSnapshot`, nickname changes and moderation-list
queries carry `requestId`. `socialResponse` repeats that ID and the action with
action-specific `data`; the browser requires both to match before resolving it.
Membership change or recovery rejects outstanding social requests. Late or
unknown social responses do not resolve another action.

The browser's `SocialRequests` and `SocialResponses` maps bind each action to its
payload and result. `requestSocial('getRoomSnapshot')` returns a `RoomSnapshot`;
`requestSocial('listRoomMembers', { offset: 100 })` returns a `RoomMembersPage`.
Mutation payloads are required, page offsets may be omitted, and snapshots take
no payload. A `socialResponse` is a discriminated union: checking `action` also
narrows `data`. Reporting creates an `open` report; resolving a report returns
only `resolved` or `dismissed`. Do not reintroduce caller-selected response casts.

Chat history is room-local memory, capped at 300 entries and 256 KiB of serialized
entries. `getRoomSnapshot` returns the retained messages visible to the current
membership, together with participants, producer state, settings and permissions.
Visibility respects join/session boundaries and private-message/ignore rules.
It is not a durable mailbox or unrestricted room-history endpoint.

Within retained history, repeating a `clientMessageId` from the same sender
session and with the same content/recipient returns the existing acknowledgement;
conflicting reuse is rejected. Eviction, full rejoin or server restart ends that
deduplication protection. An acknowledgement timeout means **unconfirmed**, not
definitely undelivered. Replay is bounded recovery, not exactly-once delivery.

## Reconnection and ownership

The browser retries closed sockets with exponential equal jitter: half to all of
a 2, 4, 8, 16, then 30-second ceiling. Successful open resets the backoff. A
continuous connection outage is bounded to two minutes; explicit retry starts a
new window and keeps the current account identity. Normal admitted disconnects can retain room
and media state for a 30-second grace period. Lobby disconnects and invalidated
credentials are cleaned up immediately; grace capacity or expired state can also
prevent recovery.

A new socket can send `reconnect` with the previous room/participant IDs and
reconnect credential. The server validates the retained session and identity;
success returns `reconnectResult` with a rotated reconnect credential. The browser
then requests a snapshot and reconciles surviving producers/consumers. Failed
recovery leads to a full rejoin and new media setup. A fresh socket or a successful
snapshot alone does not prove media has recovered.

On `serverRestarting`, an established room or lobby retains its intended room,
nickname and in-memory password, but immediately stops local media. Once signaling
returns, it performs a fresh join rather than attempting to resume transports from
the old process. The original two-minute restart deadline also bounds that join;
repeated notices do not extend it. A failed connection remains explicitly
retryable. Room deletion, user leave, and a new membership cancel the old room's
recovery. A first join that never completed remains an ordinary bounded join,
not established restart intent.

The same guest/room/viewer intent retains its unsent public draft across the new
participant ID; old guest private-conversation state is not reassigned. Account
changes and explicit departure still clear conversation state. Rejoined media
starts off and requires the user's action to publish again. This is automatic
room recovery, not uninterrupted text delivery, capture or WebRTC transport
continuity, and does not persist runtime history across server restarts.

Each socket, membership, media manager and auth operation owns its pending work.
After an `await`, code must confirm that owner is still current before adopting
results or invoking callbacks. Retired work must release its tracks/transports
without changing a replacement session. Client generation guards prevent stale
local adoption; they do not undo an already-processed server mutation. Server
handlers must independently recheck membership, authorization and sender ownership.

## Patch semantics

For `updateRoomSettings`, omission leaves a setting unchanged. `password`,
`maxParticipants` and `maxBroadcasters` additionally accept `null` to clear the
value; a supplied value sets it. For example:

```json
{"type":"updateRoomSettings","allowChat":false}
{"type":"updateRoomSettings","maxParticipants":null,"password":null}
{"type":"updateRoomSettings","maxParticipants":12,"maxBroadcasters":4}
```

These are three separate messages: change only chat availability; clear the
participant cap/password; set two caps. Other fields remain unchanged. Use an
explicit boolean to set a toggle, not `null`. Read-side `passwordProtected` is a
status flag, not a writable password. Topics use `setTopic` separately.

## Runtime validation and evolution

[The WebSocket decoder](../web/src/protocol-validation.ts) receives `unknown` after JSON
parsing. It validates the discriminant and nested participants, settings, chat,
social results and ICE/DTLS/RTP fields, then constructs the typed message. Unknown
fields are stripped. Rust null/omitted optionals normalize to frontend optionals;
explicitly nullable values remain nullable. Current ICE `address` and legacy
`ip` inputs are normalized for the client library. Invalid frames are ignored
with a payload-free diagnostic; pending requests still expire normally.

When changing the contract:

- Update Rust types/serialization, TypeScript types, the decoder and dispatch
  together. Embedded JSON settings and social payloads are part of the schema too.
- For HTTP changes, update the named method and endpoint decoder. For social
  changes, update both action maps and the action's decoder. Add valid/invalid
  runtime cases and [compile-time fixtures](../web/tests/type-contracts.ts);
  the source suite compiles those fixtures with the installed production compiler.
- Preserve omission/null distinctions and test actual Rust-shaped nested data.
  Update media fixtures when either mediasoup wire types or client types change.
- Keep decoding exhaustive. Adding a message must require an intentional decoder
  and handler decision, not a catch-all cast. New variants are not automatically
  compatible with an older browser bundle.
- Add lifecycle and invalid-boundary tests, including late replies where the
  operation can outlive its owner. Do not make browser visibility the authority
  for room permissions. See [testing](testing.md) and
  [contribution standards](../CONTRIBUTING.md).
