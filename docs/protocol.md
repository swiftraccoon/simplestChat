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
| `accountSessions` | `GET /api/auth/sessions` | `AccountSession[]`: nonsecret ID, current marker, sign-in/refresh/expiry timestamps |
| `revokeSession` | `DELETE /api/auth/sessions/:id` | `204`, no body; revokes only an owned session |
| `revokeOtherSessions` | `DELETE /api/auth/sessions/others` | `204`, no body; keeps the caller's session |
| `inbox` | `GET /api/auth/inbox` with optional `before` cursor | `{ conversations, nextCursor, retentionDays }` |
| `privateHistory` | `GET /api/auth/inbox/:peer/messages` with optional `before`, `after`, `around`, `resume`, `q`, `limit` | `ChatHistoryPage` |
| `inboxUnread` | `GET /api/auth/inbox/unread` | `{ unreadCount }`, capped at 1,000 |
| `editPrivateMessage` | `PUT /api/auth/inbox/:peer/messages/:messageId`, JSON `{content, expectedRevision}` | Updated author-owned `ChatEntry` |
| `sendPrivateMessage` | `POST /api/auth/inbox/:peer/messages`, JSON `{clientMessageId, content}` | `ChatEntry` |
| `readPrivateMessages` | `PUT /api/auth/inbox/:peer/read`, JSON `{messageId}` | `{ readMessageId: string \| null }` |
| `contacts` | `GET /api/auth/contacts` | `{accountId, contacts:[{accountId,accountName,status}]}`; status is `accepted`, `incoming` or `outgoing` |
| `requestContact` | `POST /api/auth/contacts`, JSON `{accountId}` | `204`, no body; exact account UUID only |
| `acceptContact` | `PUT /api/auth/contacts/:peer` | `204`, no body; accepts a live incoming request |
| `removeContact` | `DELETE /api/auth/contacts/:peer` | `204`, no body; decline, cancel or remove |
| `savedRooms` | `GET /api/auth/saved-rooms` | `{rooms:[{room:RoomListItem,favorite,lastVisited}]}` |
| `saveRoom` | `PUT /api/auth/saved-rooms/:id`, JSON `{favorite}` | `204`, no body |
| `updateProfile` | `PATCH /api/auth/profile` | `AccountProfile` |
| `changePassword` | `POST /api/auth/password` | `204`, no body |
| WebSocket ticket provider | `POST /api/auth/ws-ticket`, Bearer JWT and JSON `{}` | `{ ticket: string, expires_in: integer }` |
| `accountPreferences` | `GET /api/auth/preferences` | `ChatPreferences` |
| `updatePreferences` | `PUT /api/auth/preferences` | `ChatPreferences` |
| `registrationInvites` | `GET /api/auth/invites` | `RegistrationInvite[]` |
| `createRegistrationInvite` | `POST /api/auth/invites` | `RegistrationInvite` metadata plus one-time `code` |
| `revokeRegistrationInvite` | `DELETE /api/auth/invites/:id` | `204`, no body |
| `memberships` | `GET /api/rooms/memberships[?after=<room-id>]` | `{ items: MembershipItem[], next_cursor: string \| null }` |
| `roomInvites` | `GET /api/rooms/:id/invites` | `RoomInvite[]` |
| `createRoomInvite` | `POST /api/rooms/:id/invites` | `RoomInvite` metadata plus one-time `code` |
| `revokeRoomInvite` | `DELETE /api/rooms/:id/invites/:invite_id` | `204`, no body |
| `previewInvite` | `POST /api/rooms/invites/preview`, JSON `{code}` | `InviteRedemption` destination and offered role; no mutation |
| `redeemInvite` | `POST /api/rooms/invites/redeem`, JSON `{code}` | `InviteRedemption` historical acceptance result |
| `recoveryKey` | `POST /api/auth/recovery/key` | `{ recovery_key: string }` |
| `redeemRecovery` | `POST /api/auth/recovery/redeem` | `204`, no body |
| `passkeySettings` | `GET /api/auth/passkeys` | Password/recovery availability and passkey record IDs/dates |
| `passkeyAction` | `POST /api/auth/passkeys/start` | Operation-bound fresh proof or the authorized action result |
| `passkeyAuthorize` | `POST /api/auth/passkeys/authorize` | Authorized action result after fresh passkey proof |
| `passkeyEnroll` | `POST /api/auth/passkeys/enroll` | `{ kind: "added" }` or `{ kind: "replaced" }` |
| `rooms` | `GET /api/rooms` with directory query parameters | `RoomListItem[]` |
| `ownRooms` | `GET /api/rooms/mine` | `RoomListItem[]` |
| `createRoom` | `POST /api/rooms` | `RoomSettings` |
| `updateRoomIdentity` | `PATCH /api/rooms/:id/identity` | `RoomListItem` |
| `deleteRoom` | `DELETE /api/rooms/:id` | `204`, no body |

Passkey management accepts operations `{action:"add"}`, `{action:"remove",id}`,
`{action:"replace",id}` and `{action:"recovery_key"}`. Each requires a current
password or fresh, account-bound passkey assertion. Replacement returns
`{kind:"replace_registration",ceremony_id,options,recovery_key}` only after
persisting the new recovery digest. This key replaces the previous saved key.
The browser must display it and obtain explicit saved-key acknowledgment before
calling credential creation. Replacement options exclude all other account
credentials while preserving the account's user handle; ordinary Add continues
to exclude every existing credential.

Replacement registration expires after five minutes, allowing time to save the
backup before opening the provider; authentication and ordinary enrollment stay
at sixty seconds. Both enrollment variants submit `{ceremony_id,credential}` to
the same endpoint. Challenges remain one-use and account/auth-version bound.
Replacement additionally binds the exact old record and issued recovery digest.
After verified registration, one transaction rechecks these bindings, inserts
the new credential, removes the selected old record, increments the authentication
version and deletes all sessions. The `replaced` response clears the refresh
cookie; the user signs in with the new passkey. It works at the passkey limit
and without a password. The recovery digest remains available after replacement.

An authenticator may overwrite its local key during creation, before the server
can commit. Retaining the old database record does not undo that overwrite.
Keep the saved recovery key after cancellation, expiry or an unconfirmed finish;
try the new passkey if the commit may have succeeded. A concurrent recovery-key
rotation makes the prepared replacement fail closed, so recovery requires the
latest saved key. Existing recovery redemption sets a new password. A failed
preparation response can also follow a committed recovery-key rotation, but
creation has not started yet: reload and verify again before continuing.

`RoomListItem.participant_count` and `broadcaster_count` are `null`, never zero,
when the room's live state was busy at listing time; the browser renders an
unknown count rather than an empty room.

Account/public profiles include `profile_style`; profile updates require it.
Room listings and identity updates include `name_style` and `topic_style`, while
`RoomSettings` carries `nameStyle` and `topicStyle`. Each is a separate
`{color: null | palette-token, style: "accent" | "text" | "bubble"}` object.
Update inputs reject unknown colors, treatments and extra style keys. These
fields never replace a participant's chat style. Only the room owner can update
room identity/appearance; a successful save broadcasts the committed settings.

Account/profile and directory fields use snake_case. `RoomSettings`, including
the room-creation result, uses camelCase, and so does `ChatPreferences`: it is
the browser's own chat preference object (private-message opt-in, sounds, text
size, timestamp format and the ignore list) kept on the account so it follows a
signed-in viewer to other devices. The server validates it like the browser does
(a known timestamp format, at most 100 ignored account ids, never the viewer's
own) and reads a stored object leniently, so a build that adds a field never
locks a client out of its settings. Desktop notification consent stays on the
device and the chat look on the profile. `PUT` replaces the whole object; the
newest write wins. A guest, or a viewer without a token, keeps everything in
the browser. The timestamp default is `time` (visible hours and minutes); stored
timestamp choices, including `hover`, remain unchanged.

Invitations are 32-character random codes with 160 bits of entropy
(`src/invite_codes.rs`). Stored rows contain only a SHA-256 digest and a separate
random UUID `id` for management. Listing responses expose `id`, validity and use
metadata, never `code` or the digest. Creation returns the metadata plus `code`
once; a lost creation response requires revoking that metadata row and issuing a
replacement. Revocation uses the nonsecret UUID, never the capability. The digest
is deliberately unkeyed: high token entropy supplies preimage resistance without
coupling live invitation availability to signing-key rotation. It does not
protect against an actor who can modify the database.

Migration 021 invalidates outstanding invitations and their retry receipts;
already-granted memberships remain. There is one current code format and no
plaintext-column or older-code fallback.

A registration
code (`RegistrationInvite`) is single use and a week long; any account holds at
most five unused ones, and `register` accepts `invite_code`, which opens
registration while `REGISTRATION_ENABLED` is false and is spent either way
(`users.invited_by` remembers the inviter). A room code (`RoomInvite`) is
minted by a room admin for a role below their own (`role` 2–4, `uses` 1–100,
`days` 1–30; twenty unused per room) and redeemed by any signed-in account,
which gains the role in `room_roles` without ever losing a higher one; the
owner stays the owner. `redeemInvite` answers with the room and the role granted
by that redemption. A successful account/code pair has a durable receipt: retrying
with the same account returns the original result without spending another use or
restoring a role that a moderator later removed. Receipts remain replayable until
seven days after the invitation's expiry. Revocation deletes both the code and
its receipts immediately. Without a retained receipt, an unknown, spent or expired
code returns 404; a registration code also returns 404 on this room endpoint. The
returned receipt is a historical result, never evidence of current authorization.

Memberships list the rooms an account holds a role in but does not own. Every
response contains at most 100 `items`, ordered by room ID, and a required
`next_cursor` containing the last returned ID when another page exists; otherwise
it is `null`. Omit `after` for the first page and pass `next_cursor` as `after` for
the next. Room renames do not change page order. Membership additions/removals
can occur between pages; reload the first page to refresh the complete listing.
These responses carry `Cache-Control: private, no-store`. `after` is the only
supported query parameter.

The browser carries a room code in `#invite=CODE` and a registration code in
`#register-invite=CODE`. Fragments are not sent in the HTTP request target.
Secrets travel to the application only in JSON bodies, never URL paths or query
parameters. A room invitation requires sign-in, then a read-only preview and an
explicit acceptance action. Preview reveals only the room ID, label and offered
role; it creates no membership, presence or media. Acceptance grants membership
without joining. Joining remains a separate user action. A preview is advisory;
revocation, expiry or another redemption may make subsequent acceptance fail.
Both preview and acceptance use authenticated, bounded room-API admission and
`Cache-Control: private, no-store` responses.

Profile `avatar_url`, directory `topic` and directory `image_url` are required
nullable fields; null is not a missing
response. Directory counts, description and visibility flags are also required.
Unknown response fields are stripped before use. See the
[HTTP decoders](../web/src/api-validation.ts), [account handlers](../src/auth/account.rs)
and [room handlers](../src/room/api.rs).

JSON endpoints reject empty or malformed successes. Methods documented as
no-content accept `204` without attempting JSON parsing. `ApiError`
retains an unsuccessful HTTP status for UI decisions; invalid successful data
produces a fixed error without including the response body. A network failure
or invalid response does not prove a mutation was rolled back. Do not retry a
password change, recovery-key replacement, recovery redemption or room creation
blindly. Room-invite redemption may be retried with the same account/code within
the receipt window described above.

## Connection and authentication

New account passwords are normalized to NFC and must contain 15–128 Unicode
scalar characters, at most 512 input bytes, and no control characters. Signup,
password change and recovery apply the same length and common-password refusal
rules. The offline blocklist combines a pinned approximately 10,000-entry
SecLists common-credential corpus with local curated entries. It checks the
complete compatibility-normalized, case-folded comparison form, then one
candidate with at most 16 ASCII digits/punctuation removed from each end; these common
affixes do not make a known weak stem acceptable. Whole
phrases are not split into words and no character-class composition is required.
This comparison affects refusal only; the credential hash still uses NFC.
Password verification uses NFC too, without reapplying selection minimums. Hashes use standard Argon2id PHC encoding and the bounded password-work
lane; there is no raw-byte or versioned normalization fallback. Room passwords
retain their separate policy. The
[pinned corpus and MIT license](../vendor/seclists-passwords/README.md) are verified in tests and included locally at build time. No password is sent to an
external lookup service. This is a common-password list, not a complete breach
database or a claim of NIST certification.

Password and passkey signup consume the same per-address hourly budget before
checking whether an email exists. Password/recovery failures accrue only against
that account and source address cohort (IPv6 `/64`), with three free failures and
an exponential delay capped at five minutes. Failures elsewhere do not impose
an account-wide lockout. Address tables retain at most 10,000 LRU entries and
evict cold entries at capacity; strangers never share an overflow penalty.
Churn can evict a cold rate record, so these are best-effort abuse limits,
complemented by HTTP admission, bounded request bodies and independent password
work concurrency. A successful proof clears its cohort/account failure record.

The browser connects to `/ws` on the page's host, using `wss:` for HTTPS pages and
`ws:` for local HTTP. Messages are JSON text objects with a camelCase `type`
discriminant and camelCase fields.

A signed-in browser first requests `POST /api/auth/ws-ticket` with its bearer
JWT in the normal HTTP Authorization header and an empty JSON object. The
no-store response contains a 43-character base64url `ticket` (256 random bits)
and integer `expires_in` from 1 through 30 seconds, bounded by the original
access token's remaining lifetime. The browser offers `simplestchat` and
`ticket.<ticket>` as WebSocket subprotocols. The server selects only
`simplestchat`, never the ticket. JWT subprotocols and all WebSocket query
parameters are rejected; there is no alternate authenticated handshake format.
A guest can offer just `simplestchat` or omit protocols, as native load clients do.
An invalid supplied protocol or ticket never downgrades to a guest.

A ticket belongs to the account and refresh-session claims validated at issuance.
The server atomically removes it before database revalidation, then checks that
the session still exists, the account version still matches, and both the
monotonic ticket deadline and original access-token expiry remain in the future.
A failed consumed attempt requires a newly minted ticket. A successful upgrade
retains the usual ongoing expiry, renewal and session-revocation checks. Tickets
do not create sessions or extend access-token lifetime.

The process retains at most 10,000 pending ticket digests and bounded claim
records. At capacity it removes expired entries, then refuses new issuance with
503 if still full; it does not evict a live capability. Issuance uses the existing
HTTP address and authenticated-account rate budgets plus operation admission.
Tickets are process-local, disappear on restart, and require the mint and upgrade
to reach the same process in any future multi-node deployment.

Clients must mint a fresh ticket for every connection attempt and fence the
asynchronous response against account changes, disconnects and superseded
attempts. Tickets must never enter URLs, logs, persistent browser storage or
saved examples. Their short lifetime and one-use semantics reduce replay from
handshake-header exposure; they remain sensitive capabilities until consumed or
expired and do not make header logging safe.

Browser upgrades must satisfy the server's Origin policy. Use `ALLOWED_ORIGINS`
for explicit deployment origins; its unset development behavior permits matching
loopback origins. Origin-less native clients are handled separately. See
[configuration](configuration.md) for deployment settings.

Authentication comes from the handshake, not from participant IDs in messages.
The server also enforces JWT expiry and account revocation on open sockets.

Access JWTs always include `auth_version` and a UUID `sid`. Authorization requires
the current account version and the named, unexpired session belonging to that
account; there is no sessionless JWT contract. Refresh tokens are opaque to the
browser and use only the secure HttpOnly `__Host-refresh_token` cookie at `Path=/`
with `SameSite=Strict`. The server accepts its single current `v1n` format (two
43-character unpadded base64url secrets encoding 32 bytes each), never a raw UUID
or another family format. Issuance and sign-out set or clear only this cookie.
Migration 022 invalidates existing sign-ins once, preserving accounts,
password/passkey credentials, recovery keys and memberships. Affected clients
must sign in again; no token transition or fallback is provided.

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
Logout and identity changes still leave and replace the socket. A refreshed token
preserves membership only when its account ID matches the current account.
Same-origin tabs publish random revision hints over BroadcastChannel and storage,
then reconcile identity from the shared HttpOnly session cookie. Hints contain no
account IDs or credentials. Interactive cookie mutations and refresh share a
Web Lock when available; epoch guards retire stale results when coordination is
unavailable. A received hint retires pending dialogs before reconciliation; focus
and visibility changes check for missed hints.

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
| Room/moderation control | Generated `requestId` and `roomControlApplied`; 25 seconds, followed by snapshot reconciliation on an unconfirmed result |
| Room join | Next `roomJoined`, `lobbyWaiting`, `roomPasswordRequired` or `error`; 10 seconds |
| Social action | Generated `requestId` plus matching `action`; 10 seconds, at most 32 pending |
| Chat send | `clientMessageId` reconciles the optimistic entry with an acknowledgement; 12 seconds before marking delivery unconfirmed |

An `error` carries a human-readable `message`. An error with a `requestId` rejects
only that pending request. Errors without IDs are connection or uncorrelated
command notifications and cannot settle a pending media/reconnect request; they can reach
the join handler. `roomPasswordRequired` is a distinct retry/prompt result, while
`roomClosed` is terminal room state. `serverRestarting` is a separate temporary
process-shutdown event with a human-readable `reason`; it is not room deletion.
Do not infer machine-readable error codes from message text.

The browser assigns each acknowledged media/reconnect command a fresh `requestId`
and retains its sequence across room changes and reconnects. IDs contain 1–64
ASCII letters, digits, hyphens or underscores. The server echoes the ID on direct
success and error replies, including malformed-command and rate-limit errors
when the envelope contains a valid ID. The envelope is optional: the current native
load generator sends ID-less setup commands, waits for their typed replies in
sequence, and associates subscription replies with resource identities. Present
invalid IDs are rejected before dispatch. The browser requires
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
`send` does not queue messages while disconnected. An ID-less reply cannot
satisfy a browser request, regardless of its response type.

The media manager retains each control until its correlated acknowledgement:
producer/consumer pause or resume, closure, layer selection and ICE restart.
Only one command per resource/state category is in flight; newer choices coalesce
behind it. A socket loss leaves unacknowledged controls pending, including commands
whose native WebSocket send succeeded before the server received them. It applies
the latest pending state after the same room session
resumes and its snapshot is reconciled (or snapshot retrieval fails). Superseded
controls and resources revoked by the snapshot are discarded, and a snapshot can
confirm that a locally closed producer is already absent. Leaving or doing
a fresh join discards the old manager's pending controls. Creation and capture
requests are never queued for replay. A second disconnect retires the previous
recovery attempt without letting it close or resume the new socket's media.
An explicit rejection reports the failed change and discards that command.
A timeout reports the unconfirmed change and keeps it for the next recovery:
the socket can still appear open after delivery has stopped. Neither failure
retries in a loop, and a newer user choice can proceed immediately.
Late results from a retired socket or resource cannot acknowledge a newer attempt.

`closeConsumer`, `closeProducer` and `setConsumerPreferredLayers` reply with
`mediaControlApplied` and the request ID after successful processing. This reply
is emitted only for commands carrying an ID. Current fire-and-forget callers
include native generator layer updates and browser cleanup of a publication or
consumer that finishes after local media teardown. Those calls retain no pending
acknowledgement and receive no `mediaControlApplied` response.
Layer acknowledgement means the server accepted the preference, not that the
selected layer has become available or been decoded.

If signaling is lost during initial transport setup, the browser closes its
partial local media manager. Recovery first reclaims the admitted session, then
joins again so the server retires any transports whose creation replies were
lost. Rejoining creates transports without starting camera, microphone or screen
capture. Ordinary unsupported-browser setup errors remain chat-only; a rejected
initial admission is never treated as automatic rejoin intent. A second loss
during fresh admission cancels its waiter immediately, and late setup results
cannot report a closed or replaced socket as connected.

Room settings, topic, role, voice, lobby and moderation commands carrying a
request ID return `roomControlApplied` only after their operation completes.
Legacy commands without an ID remain silent. The browser prevents duplicate
pending actions, retains failed form edits, and reads current room state after
an uncertain result. A timeout is not rollback; checking the snapshot does not
establish whether an absent admission/voice notification was delivered. Retrying
is a separate user decision, never automatic replay of a moderation mutation.

## Join, lobby and media

1. Send `joinRoom` with `roomId`, `participantName` and an optional password.
   The server determines authenticated identity, role and access. A successful
   `roomJoined` supplies the local participant ID, reconnect credential, existing
   participants/producers, role and optional persisted-room settings.
2. `lobbyWaiting` is not admission: do not initialize media while waiting.
   Moderators receive `lobbyJoin`. Admission sends `lobbyAdmitted` followed by
   `roomJoined`; denial sends `lobbyDenied`. `lobbyWaiting` includes
   `participantCount` and `moderatorCount`; `lobbyStatus` updates those counts as
   connected membership or roles change. Disconnected grace sessions do not
   count as available moderators. Disconnect pushes are best effort: a busy room
   lock is allowed at most 100 ms after socket closure, and draining skips this
   notification. Later counts still exclude closed queues. A connected moderator
   is not an approval or a response-time promise.
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
   releases a receiver that is no longer needed. Correlated closure receives
   `mediaControlApplied`; repeated closure is harmless and only the current
   session's consumer is affected.

Producer pause/resume/close events describe shared publishing state. Consumer
pause/resume and preferred layers affect that receiver's subscription; local
playback volume is not producer moderation. `restartIce` / `iceRestarted` update
an existing transport and are distinct from reconnecting signaling; `iceRestarted`
carries fresh `iceServers` because the TURN credentials minted at transport
creation expire after `TURN_TTL`, and the browser installs them before it
regathers candidates. An empty server list needs no configuration update.
If the browser handler cannot update nonempty ICE-server settings (as with
mediasoup-client's Firefox handler), the browser closes the old media manager
and rejoins with fresh transports. It first reclaims the session if signaling
is disconnected or recovery is pending. Capture remains off until explicitly
enabled; late unsupported-update results cannot rebuild a replaced session.

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

Reconnect replay is room-local memory, capped at 300 entries and 256 KiB of
serialized entries. `getRoomSnapshot` returns messages visible to the current
membership with participants, producer state, settings and permissions. Replay
respects join/session boundaries and private-message/ignore rules.
Before recovering, the browser captures the IDs of already-confirmed public
rows. The first recovery snapshot discards those captured rows if no longer
retained by the server, so a missed removal cannot leave older cached text
visible. Public rows arriving after that capture, PMs and uncertain outgoing
attempts are preserved; ordinary later snapshots do not prune unrelated rows.

Saved history is separate PostgreSQL data. A room owner uses `setRoomHistory`
with `retentionDays` (`0`, `1`, `7`, `30`, `90`); the default `0` is Off. Settings
expose `historyRetentionDays`. Enabling history saves future public messages,
including guest public messages, and admits current members to older retained
messages regardless of their join time. Disabling purges saved public messages
and read cursors. Shortening retention deletes expired messages and shortens
remaining lifetimes; extending retention applies to future sends only.

`getChatHistory` accepts optional `before`, `after`, `around`, `resume`, `q` and
`limit` and returns `{messages, nextCursor, newerCursor, firstUnreadMessageId,
readMessageId, retentionDays}`. Messages are chronological; opaque cursors order
pages by server timestamp and message UUID. `nextCursor` retrieves older messages
through `before`; `newerCursor` retrieves later messages through `after`. `limit`
is 1–50 (default 50). `around` names a retained message in this exact conversation
and loads bounded context on either side. `resume: true` loads context around the
first unread incoming message, or the latest page when nothing is unread. At most
one of `before`, `after`, `around` and `resume: true` may be supplied. Search `q`
accepts 3–128 characters, examines the newest 10,000 retained messages, and may be
combined only with `before` pagination. A search result's ID can then open its
surrounding conversation using `around` without `q`.

`markChatRead` takes `messageId`; the signed-in viewer's read cursor moves forward
only. Loading history does not mark it read. Both actions require current room
membership. History Off returns an empty saved page; it does not extend runtime
replay. The same positioning and read rules apply to authenticated PM history.

Account-to-account PMs are retained for 90 days. Authenticated HTTP routes are
`GET /api/auth/inbox`, `GET|POST /api/auth/inbox/:peer/messages` and
`PUT /api/auth/inbox/:peer/read`. Message pages have the same history shape;
inbox pages return `{conversations, nextCursor, retentionDays}`. Each conversation
includes the peer identity/name, last message and an unread count capped at
1,000. POST takes `{clientMessageId, content}` and PUT takes `{messageId}`.
An HTTP send requires either an existing retained account conversation or an
accepted contact relationship. Both room and HTTP account PM sends honor the two accounts' saved
ignore/PM preferences under database row locks. Room-session preference changes
also wait for in-flight durable sends before acknowledging, so an opt-out cannot
overtake an accepted send. A known-success HTTP retry returns its existing receipt
without redelivery, even after preferences change. Inbox delivery works while the
peer is offline. Guest PMs remain ephemeral. Saved writes commit
before room delivery/acknowledgement. An uncertain database commit quarantines
the room and reports `messageRetryResult` with `storage_unconfirmed`, never a
definite send rejection. Account identity scopes every inbox route; knowing another conversation's peer or message ID grants no access.

Contacts require explicit acceptance by the recipient. Contact links use
`#contact=<account-UUID>`; the browser scrubs the fragment and offers a separate
request action after sign-in. These identifiers grant no authority and are not
email or account-name search. Missing, opted-out and ignoring recipients receive
the same `204` request response. Either account's PM opt-out/ignore prevents a new
request or acceptance. Account rows are locked in UUID order before the current
session is rechecked. Each account has at most 100 accepted contacts plus live
pending requests and may issue at most 30 new requests per day. Pending requests
expire after 14 days; declining, canceling or removing a relationship prevents
another request for that pair for 30 days. Removing a contact does not delete
retained messages or replace the independent ignore control.

Saved rooms belong to an account, across its devices and sessions. Up to 100
favorites and 50 additional recent rooms are retained. Recents are recorded only
after a successful authenticated admission to a persisted room, never for an
attempted join or ad-hoc room. Saved shortcuts do not bypass room admission.
Unlisted room details appear only while the account currently owns the room or
holds a room role; remembered access alone is insufficient. Account deletion and
room deletion cascade their shortcuts. Every discovery route requires a current
session and returns private, non-cacheable responses.

Each participant has a chat look, `chatStyle`: `{ color, style }`. `color` is one
of sixteen palette tokens (`CHAT_COLORS` in `src/signaling/protocol.rs`, mirrored
by `CHAT_PALETTE` in `web/src/avatar-colors.ts`) or `null` for the automatic color
derived from the name; `style` is `accent`, `text` or `bubble`. `joinRoom` may
carry a guest's `chatStyle`; an account's saved look (`users.chat_color` and
`users.chat_style`) takes its place, and `roomJoined.yourChatStyle` reports the
look applied. The `setChatStyle` social action changes it, saving an account's look
before anyone sees it, and the room broadcasts `chatStyleChanged` to everyone,
the sender included. Looks travel on `ParticipantInfo`, `participantJoined`,
`ChatEntry` and `chatReceived`, so a message keeps the look it was sent with.
The server rejects a `setChatStyle` color outside the palette, ignores one on a
join, and treats an unknown style as `accent`. The browser shows a color token it
does not know as the automatic color and an unknown style as `accent`; neither
drops the message.

`joinRoom.participantName` is the requested room-local name for both guests and
accounts. The browser uses the account profile name as a default when no name
has been entered; an edited name takes precedence without changing the profile.
`roomJoined.yourName` is the room-local label assigned to a joining participant.
Guests and accounts both receive the smallest free numeric suffix when another
participant or lobby entry holds an equivalent label, for example `Maya (2)`.
Account profile names remain unchanged. `changeNickname` refuses a collision.
Comparison uses compatibility normalization (NFKC), full Unicode case folding,
and removal of Unicode default-ignorable characters; displayed spelling is
preserved. Thus canonical accents, width variants, case variants and inserted
joiners cannot bypass duplicate or reserved-`You` checks. Empty visible labels,
control characters, bidi overrides/isolates, zero-width spaces and byte-order
marks are rejected consistently by creation and room-label updates. Joiners,
variation selectors and direction marks remain usable in legitimate displayed
scripts and emoji, but do not create distinct comparison identities.

Names are not authorization identifiers and this comparison does not promise to
detect all cross-script confusables. The server-issued full participant UUID is
the identity used for chat, replies, private-message targeting and moderation.
An account uses its stable user UUID; a guest uses a random connection identity
retained across grace reconnection. The browser displays names without participant
IDs or account/guest labels in the roster and chat, including private conversation
tabs and quoted replies. Identity details remain available in moderation
confirmations; the full UUID is always the action and conversation lookup key.

A send may name the retained message it answers with `replyTo` (a `messageId`).
The server quotes that message itself as `ChatEntry.replyTo` (`ChatReplyRef`: its
`messageId`, sender and a one-line excerpt of at most 140 characters), so the
quote outlives the original's eviction and cannot be forged. The original must
still be retained, visible to the sender and in the same conversation (public
with public, or the same private pair), else the send is rejected. A retry must
repeat the same `replyTo`; a differing one is a conflict like differing text.

`reactToMessage` (`messageId`, `emoji`) toggles the sender's reaction on a
retained message it can see; `emoji` must be one of `REACTIONS` in
`src/signaling/protocol.rs` (mirrored by `CHAT_REACTIONS` in
`web/src/social-chat.ts`). The response and a `messageReactions` broadcast to
everyone who can see the message carry the message's full reaction list, oldest
first, as `{ emoji, participantIds }`. A message holds at most 64 reaction
records, and reactions count toward the history byte budget. Reactions are not
part of delivery receipts. Reactions on saved messages are persisted; adding a
reaction through room chat still requires the message in current runtime replay.

`removeChatMessage` takes `{requestId, messageId}` and requires Moderator+.
It targets only public messages from the current room, including saved messages
outside runtime replay. The response carries `{messageId, removedAt}` and the
room receives `chatMessageRemoved` with the same fields. A removed `ChatEntry`
keeps its message/sender/time identity, has `removedAt`, empty `content` and
reactions, and no `replyTo`. Quoted excerpts in other retained messages become
`Message removed`. Removal scrubs runtime replay, retry receipts and the database
before publication; replay or a same-attempt retry cannot restore content. Its pin
is removed in the same persistence transaction. New replies, edits, pins and
reactions to a removed message are refused. Repeating removal keeps
the original marker and does not duplicate the moderation event. PM content is
never exposed to this operation.

`ChatEntry` and `chatReceived` carry a monotonic integer `revision`, initially 0,
and edited messages carry `editedAt` as an RFC3339 timestamp. `editChatMessage`
takes `{requestId, messageId, content, expectedRevision}` and returns `{message}`.
Only the author may edit; guests additionally require the original room
membership. Normal text limits and room chat permissions still apply. The edit
preserves the message ID, send time, recipient, original appearance and delivery
receipt. A mismatched revision is rejected; retrying the same content from the
immediately preceding revision returns the committed result. Removal is terminal
regardless of revision. Edits update existing quote excerpts and saved send
receipts. New quoted sends and edits serialize per conversation so a new reply
cannot commit a stale excerpt after the edit.

Visible recipients receive `chatMessageEdited` with `{message}`. Private events
remain confined to the two participants. Account PMs outside room replay use
`PUT /api/auth/inbox/:peer/messages/:messageId` with `{content, expectedRevision}`;
the server derives the conversation, checks ownership/current session and saved
PM consent, commits, then reconciles live room sockets. Clients compare revisions
when reconciling asynchronous history, events and receipts. An older body must
never replace a newer edit or a removal tombstone.

`getPinnedMessages` takes `{requestId}` and returns `{messages}`.
`setPinnedMessage` takes `{requestId, messageId, pinned}` and returns the same
shape; mutations require Moderator+ and target public messages in the current
room. Rooms hold at most three pins, newest pin first. Changes broadcast
`pinnedMessagesChanged` with `{messages}`. Ignored authors are filtered from a
viewer's list. Without saved history, pins last only for that room runtime. With
saved history enabled, pins survive restarts and share the original message's
expiry; they never extend retention. Removing a message or disabling saved
history removes its persisted pins. Expired pins are excluded on reads, and the
browser refreshes visible pinned messages periodically and after settings changes.
Migration 027 adds bounded pin slots; migrations 028–029 add indexes for unread
counts and quote updates. Existing chat bodies begin at revision 0 without
changing their text or retention.

Every sanction and report decision leaves an entry in the room's moderation
history, written in the same transaction as the change it records: `kick`,
`ban`, `unban`, `cam_ban`, `cam_unban`, `text_mute`, `text_unmute`,
`report_resolved`, `report_dismissed` and `message_removed`. Message removal
records the removed message ID without its content. `listModerationEvents` (Moderator+,
`offset` pages of 100, newest first) returns `ModerationEventEntry` values: who
acted, whom it concerned (`targetAuthenticated` says whether `targetId` is an
account), the reason, a ban's `expiresAt`, and the `reportId` it answered.
`targetIp`, the sanction's address cohort, reaches the owner only; moderators'
listings never carry it. A persisted room keeps its newest 1000 entries in
PostgreSQL; an ad-hoc room keeps 200 in memory. `kick` and `ban` may name the
open report they answer with `reportId`: the server resolves that report with
the sanction and links the entry to it, and a report from another room is
refused. `ReportEntry.outcome` is the newest linked entry (`action`,
`createdAt`), so a resolved report shows what it led to. A six-hourly sweep
clears a target's address after `MODERATION_ADDRESS_RETENTION_DAYS` and removes
entries, closed reports and expired sanction rows after
`MODERATION_HISTORY_RETENTION_DAYS`; open reports and live sanctions stay.

`typing` (optional `targetParticipantId`) says the sender is composing, and
the server relays it as `participantTyping` to whoever would receive the
message: everyone who does not ignore the sender for public chat, only the
target for a private conversation (and only if they accept private messages
and neither ignores the other). A sender who cannot chat relays nothing, and
the server forwards at most one notice per sender every two seconds. The
browser sends one while composing at most every 2.5 s and shows "Alice is
typing…" beneath the visible conversation for four seconds per notice; nothing
is retained or acknowledged.

Modern chat sends include a strictly increasing safe-integer `sequence` alongside
`clientMessageId`. The room snapshot supplies a separate `chatSessionId` for this
membership; it survives grace reconnection and changes on a fresh join. A
`retryChatMessage` carries that ID and the original sequence, message ID, content
and optional recipient. It never generates a new message ID for the old attempt.

Accepted-message receipts are separate from visible history, bounded to 512
entries / 512 KiB per room and 128 per membership, retained for up to five
minutes. Earlier FIFO eviction at member/room count or byte limits admits new
messages without throttling chat for confirmation storage.
Matching retained receipts return the existing acknowledgement without delivering
again. A per-membership sequence watermark survives receipt/history eviction;
missing old receipts yield `messageRetryResult` with `outcome: "unknown"` and a
finite reason, never another broadcast. A public retry newer than the watermark
may make its first delivery. A private retry without a receipt is lookup-only:
the server cannot establish that the recipient is still the original membership.
Legacy sends without a sequence retain only bounded-history/receipt deduplication.

The UI distinguishes confirmed, rejected and unconfirmed sends, and lets users
reconcile/retry a retained uncertain attempt or copy it back to a draft. Explicit
new delivery from a draft is a new decision. Full rejoin or server restart retires
retry ownership; these in-memory receipts do not become durable retry handles.
Retained messages remain accessible through saved-history routes. An
acknowledgement confirms server acceptance, not that every recipient read it.
These bounds are not an exactly-once-delivery guarantee.

## Reconnection and ownership

The browser retries closed sockets with exponential equal jitter: half to all of
a 2, 4, 8, 16, then 30-second ceiling. Successful open resets the backoff. A
continuous connection outage is bounded to two minutes; explicit retry starts a
new window and keeps the current account identity. Normal admitted disconnects can retain room
and media state for a 30-second grace period. Lobby disconnects and invalidated
credentials are cleaned up immediately; grace capacity or expired state can also
prevent recovery.

A socket has a 64-message outbound application queue. If an essential room event
or request response cannot be queued, an independent control signal retires that
socket and attempts a `1013` close within one second. It cannot report the missing
event through the already-full queue. The browser follows its normal reconnect
and snapshot path, so it does not remain indefinitely on a partial room view.
Speaker, audio-level, layer and bandwidth hints remain disposable. Existing
`outbound_queue_full` metrics count essential enqueue failures; repeated failures
coalesce into one retirement signal per connection.

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
continuity. Runtime replay is lost on restart; configured public history and
account PMs remain available through the saved-history interfaces.

“Refresh incoming media” explicitly retires existing receive consumers, awaits
closure acknowledgement, then resubscribes to still-current remote producers.
It preserves local capture/mute state and reapplies viewer hide, quality and size
preferences. Completion confirms subscription setup, not decoded frames. It is
separate from transport-failure ICE/rejoin recovery and never captures a device.

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
Non-null participant limits must be 1–10000 and broadcaster limits 1–1000;
broadcasters cannot exceed a configured participant limit. Zero is rejected.

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

## Private-message push notifications

Authenticated account sessions can opt in to generic Web Push alerts. These
HTTP routes require the same current bearer token and live-session validation as
other account operations; responses use `Cache-Control: no-store`:

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/auth/push` | No body | `{ "publicKey": "…", "enabled": false }` |
| `PUT /api/auth/push` | `{ "endpoint": "https://…" }` | `204 No Content` |
| `DELETE /api/auth/push` | No body | `204 No Content` |

`enabled` describes the current authenticated session, not every device on the
account. `publicKey` is the uncompressed 65-byte P-256 VAPID public key encoded
as unpadded base64url. The signing key is generated once and retained in
PostgreSQL's `push_keys` table; ordinary database backup/restore and server
migration preserve it and existing subscriptions. The private key is never
returned to a browser.

Registration accepts only the endpoint field and stores one subscription per
session, associated with that account's current authentication version. An
endpoint can move between sessions of the same account. Reusing an endpoint
bound to another account is rejected: the browser must unsubscribe and obtain a
fresh subscription after the user's explicit enable action. Browser-side Web
Locks and a nonsecret account/lease marker prevent delayed cleanup in one tab
from unsubscribing a newer account's registration. Tokens, chat text and endpoint
URLs are not persisted by the service worker.

Endpoints are bounded to 2048 bytes and HTTPS on the supported browser-provider
hosts: `fcm.googleapis.com`, `updates.push.services.mozilla.com`,
`web.push.apple.com`, or a subdomain of `notify.windows.com`. Credentials,
fragments, nondefault ports and empty paths are rejected. Outbound delivery uses
no system proxy and follows no redirects. VAPID requests have an empty body:
provider delivery cannot carry sender identities or message contents. The worker
shows **New private messages** and opens the fixed app inbox destination.

New durable account PMs coalesce into a bounded delivery queue; guest PMs do not.
Before delivery, the worker checks live session/account authorization and whether
an unexpired, unremoved PM is still unread. A message read during the initial
coalescing window need not produce an alert. Failures use bounded retries;
expired provider endpoints are removed. Queue bookkeeping never changes message
acceptance or retries the chat message itself.

Logout and individual/bulk session revocation delete session-owned subscriptions
through the session foreign key. Account authentication-version changes also
make old subscriptions ineligible. Disabling affects only the current session;
other devices retain their independent opt-in. Provider delivery is best effort,
and a notification already accepted by a provider may arrive after a read or
sign-out. The app does not promise immediate recall, offline chat access, or
end-to-end encryption for stored messages.

## Account notification policy

`GET /api/auth/notification-preferences` returns `{privateMessages,mentions,
quietHours,conversations}`. `quietHours` is null or `{startMinute,endMinute,
timeZone}` (minutes 0–1439, unequal; IANA zone). PUT on that endpoint accepts the
three global fields. PUT `/api/auth/notification-preferences/conversations/{peer}`
accepts `{muted,snoozedUntil}`; the timestamp is null or a future RFC3339 instant
within 30 days. False/null deletes the override. Both writes return the complete
policy. There are at most 100 overrides per account; defaults allow PM/mention
alerts with no quiet period. Device browser permission is separate.

Conversation entries have `{peerId,muted,snoozedUntil}`. Quiet intervals include
their start and exclude their end; midnight-crossing intervals and daylight-saving
changes follow the saved zone. Suppressed alerts are discarded, not replayed when
the quiet period ends. Browser policy refreshes on focus and every 30 seconds while
visible; signed-in foreground alerts wait for the first successful load and pause
if that snapshot is more than 90 seconds old. Messages and unread cursors are
unaffected. Providers may still deliver a notification already accepted before a
policy change.
