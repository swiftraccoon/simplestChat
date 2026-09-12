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
Refreshing an HTTP session updates the browser's token for its **next** handshake;
it does not reauthenticate the existing WebSocket.

The server limits inbound frames/messages to 64 KiB and closes connections after
five minutes without an inbound frame. WebSocket ping/pong is handled by the
socket implementation; there is no application `ping` message. Rate limits and
bounded outgoing queues also apply. A connected WebSocket means neither room
admission nor working media.

## Requests, replies and events

| Operation | Correlation and browser deadline |
| --- | --- |
| Generic `SignalingClient.request` | First pending request matching the response `type`; 5 seconds by default |
| Room join | Next `roomJoined`, `lobbyWaiting`, `roomPasswordRequired` or `error`; 10 seconds |
| Social action | Generated `requestId` plus matching `action`; 10 seconds, at most 32 pending |
| Chat send | `clientMessageId` reconciles the optimistic entry with an acknowledgement; 12 seconds before marking delivery unconfirmed |

An `error` carries a human-readable `message`. It rejects the first pending generic
request regardless of that request's expected response type; an unclaimed error
can reach the join handler. `roomPasswordRequired` is a distinct retry/prompt
result, while `roomClosed` is terminal room state. Do not infer machine-readable
error codes from message text.

Generic requests have **no request ID**. Concurrent requests expecting the same
response type cannot be distinguished by the client. Transport setup therefore
runs sequentially, and the room client queues consume transactions. Removing a
timed-out resolver prevents that resolver from consuming a later reply, but does
not cancel server work: a late reply can still match a newer request of the same
type. Avoid overlapping ambiguous transactions or assuming a retry proves the
first operation did not happen. Reliable independent retries would require a
coordinated wire-level correlation change.

Valid replies are offered to pending generic requests before event dispatch.
Unmatched events update membership, moderation, chat or media state. Malformed
messages cannot resolve requests. Socket close/disconnect rejects pending generic
requests; callbacks from a replaced socket are ignored. `send` does not queue
messages while disconnected.

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

Producer pause/resume/close events describe shared publishing state. Consumer
pause/resume and preferred layers affect that receiver's subscription; local
playback volume is not producer moderation. `restartIce` / `iceRestarted` update
an existing transport and are distinct from reconnecting signaling.

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

The browser retries closed sockets after 2, 4, 8, 16, then at most 30 seconds;
successful open resets the backoff. Normal admitted disconnects can retain room
and media state for a 30-second grace period. Lobby disconnects and invalidated
credentials are cleaned up immediately; grace capacity or expired state can also
prevent recovery.

A new socket can send `reconnect` with the previous room/participant IDs and
reconnect credential. The server validates the retained session and identity;
success returns `reconnectResult` with a rotated reconnect credential. The browser
then requests a snapshot and reconciles surviving producers/consumers. Failed
recovery leads to a full rejoin and new media setup. A fresh socket or a successful
snapshot alone does not prove media has recovered.

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
