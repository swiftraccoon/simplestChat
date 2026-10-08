# Browser client

TypeScript/Vite UI using mediasoup-client. Commands below run from the repository
root. The manifest specifies the minimum Node version; CI pins its exact Node
release. Dependencies and development tools resolve through the lockfile.

The runtime imports `Device` directly from the shipped
`mediasoup-client/lib/Device.js` file; other mediasoup imports are type-only.
Version 3.23.1 exposes no `Device` package subpath, so this is an explicit relative
import into the exactly pinned dependency. Its CommonJS barrel also exports test
helpers and embeds public fixture credentials even with a named import. The
direct import keeps the same `Device.factory()` implementation and browser
handler selection. The small Vite resolver also replaces the single barrel import
in `RemoteSdp.js`, whose only use is the SDP origin's version string, with that
same pinned version. It verifies the installed package version and complete
`RemoteSdp.js` hash first; changed dependency bytes stop the build for review.
No handler or transport implementation is replaced. Dependency updates must
verify the shipped module/types, media tests, and production bundle contents.

## Build and develop

```sh
npm ci --ignore-scripts --prefix web
build/check.sh --web
```

This checks typed lint, formatting, source tests and both TypeScript projects,
then produces the production bundle and checks its
[asset-size budgets](../docs/performance.md#web-asset-budgets).
Run `npm --prefix web run format` to apply
the committed formatter. See [contribution standards](../CONTRIBUTING.md) for
async ownership, boundary validation and documentation requirements.

The Rust server serves `web/dist` relative to its working directory; run it from
the repository root. Rebuild after source changes when testing this production
bundle. Do not rebuild during an in-progress browser test.

For hot reload, keep a separately configured local Rust server on port 3000:

```sh
npm --prefix web run dev -- --host 127.0.0.1
```

Open the localhost URL printed by Vite. Both `/api` and `/ws` proxy to the local
Rust backend; Vite supplies assets, not authentication or media services.
PostgreSQL/JWT/registration setup still belongs to the backend. If changing its
port, update both proxy targets in [vite.config.ts](vite.config.ts).

Passkey configuration must match the **browser** origin (including Vite's port),
not the upstream port. Use localhost consistently for the relying-party ID and
origin. WebSocket origin checks remain server-enforced; configure the exact local
origin if your backend setup requires it. Vite preview serves built assets only;
use the Rust server for full production-bundle testing.

For complete guest/account setup and LAN ICE addressing see
[development](../docs/development.md). Do not expose a Vite server publicly.

## Module map

| Module | Responsibility |
| --- | --- |
| `src/main.ts` | Application wiring, room shell and layout |
| `src/video-layout.ts` | Uniform media cells sized to the available stage |
| `src/ui.ts` | Text-safe controls, dialogs and named HTTP API methods |
| `src/participant-hovercard.ts` | Shared participant cards for roster and chat names, profile loading ownership and pointer/keyboard/touch interaction |
| `src/appearance.ts` | Independent profile-card and room-heading appearance controls, previews and palette-only rendering |
| `src/auth.ts`, `account-session-sync.ts`, `community-ui.ts` | Identity/session refresh and cross-tab reconciliation, profiles, recovery, room management |
| `src/account-security.ts`, `account-sessions.ts` | Shared account-mutation ownership, passkey/recovery management and signed-in session controls |
| `src/signaling.ts`, `protocol.ts` | WebSocket lifecycle and typed server protocol |
| `src/validation.ts`, `api-validation.ts`, `protocol-validation.ts` | Shared decoder primitives and HTTP/WebSocket response validation |
| `src/room.ts`, `room-navigation.ts` | Membership, confirmed room controls, lobby status and explicit URL navigation |
| `src/media.ts` | Transports, capture, producers and consumers |
| `src/media-controls.ts`, `audio-output.ts` | Private device preview, live hardware lists, speaker selection/test and viewer-local playback |
| `src/layer-cap.ts` | Simulcast layer a remote tile can use at its rendered size, with hysteresis |
| `src/settings-dialog.ts` | Shared settings tabs and native-dialog dismissal |
| `src/social-chat.ts`, `chat-store.ts` | Room/PM conversations, composer, bounded replay, removal tombstones and preferences |
| `src/chat-history.ts` | Saved room history, retention controls, account PM inbox, search and shared read positions |
| `src/*.css`, `index.html` | Layout, component styles and initial document |
| `public/help.html`, `help.css` | Zero-JavaScript user help, copied into the production output |
| `scripts/check-bundle.mjs`, `bundle-budget.json` | Aggregate raw/gzip asset limits enforced by every production build |

Rust signaling types in [src/signaling/protocol.rs](../src/signaling/protocol.rs)
are the other half of the wire contract. Update both sides together, including
camelCase room settings embedded in JSON values.
See the [protocol guide](../docs/protocol.md) for sequencing, correlation limits,
reconnect/replay and runtime validation.

Use the named `api` methods for profile, recovery and room HTTP requests; the
method owns the route, request body and result decoder. Social requests derive
their payload and response from the action instead of accepting a caller-selected
result type. Both boundaries start with `unknown`, strip unrecognized fields and
preserve the contract's nullable values. Add a decoder and valid/invalid tests
when extending either boundary, not an assertion at the UI callsite.

Joining does not capture or publish. Preview is local until explicit publication.
Personal settings share one dialog: layout and talk mode apply immediately;
device changes apply on Save, updating active capture without enabling inactive devices.
Viewer mute/volume/hide must not change what anybody else receives. Async media
and account work can outlive a dialog or session; stale completion must not attach
tracks or account data to a replacement session.

Room links and browser history select a room; joining remains explicit. Invitations
use 32-symbol secrets in fragments (`#invite=` for rooms, `#register-invite=` for
registration), scrubbed before room navigation. Room invitations require signed-in
preview and explicit acceptance through JSON request bodies. Acceptance selects
the room; joining is a separate action. Management lists and revocation use nonsecret
invitation IDs; a newly created secret is shown only once. Account-room Join buttons
retain their explicit join intent until signaling and any departure finish. A newer
destination, edited room ID or account change retires asynchronous invitation work
and pending joins.
Home, Leave and lobby Cancel clear the selected room URL. No URL change starts capture.
Room settings retain failed edits and offer an explicit retry after checking
current server state. A connected lobby moderator is availability information,
not a promise of admission.

Output selection is feature-detected and applies to owned playback elements.
Output device selection remains tab-local; saved camera/microphone IDs and capture
preferences use browser-local storage. Browsers without output routing use the
system output. Speaker testing plays a short local tone and releases its URL/timer on
completion or dialog departure. Device-list changes never request capture.
The camera and microphone controls request access directly when explicitly
activated; opening Settings or joining a room does not capture. Settings offers
separate private **Test camera** and **Test microphone** actions beside the
selectors to grant access and refresh device names before saving preferences.
Saving settings changes active devices but does not turn an inactive device on.
Camera publication, recapture and preview share `captureMedia`: a
`NotReadableError` with requested resolution/frame-rate preferences permits one
retry without those quality constraints. The retry preserves an exact selected
camera and any audio constraints. Permission denial and other errors do not
trigger it; cancelled work cannot begin a retry, and late streams are stopped.
Screen sharing distinguishes cancellation/permission failure from setup errors,
and reports whether optional screen audio is included, unavailable or ended.

The room uses one header; call controls sit below the video column so the desktop
People and chat panels retain their full height. Conversation buttons above chat
switch between Public chat and private messages, with unread counts and separate
drafts. The composer grows to six lines on desktop and three at phone widths,
then scrolls. On devices without touch input, Enter sends and Shift+Enter adds a
line. Touch-capable devices show Send and use Enter for new lines; Ctrl/Cmd+Enter
sends in either mode. An open mention picker can use Enter to complete a name
before sending.

Chat options includes **Room history**. Owners choose Off (the default), 1, 7,
30 or 90 days for persisted rooms. Everyone admitted to a room can read its saved
public messages, including messages from before they joined. Turning retention
off deletes saved public history; shortening it deletes messages outside the new
period. Longer retention applies to future messages and does not recover or
extend older saved messages.

The signed-in header's **Messages** opens the account inbox. PMs between accounts
are retained for 90 days, across rooms, devices and server restarts. Guest PMs
remain bounded room-session data. Start a conversation together in a room; an
existing account conversation can continue from Messages while either person is
offline, subject to PM opt-out and ignore preferences. Saved history pages hold
at most 50 messages, and search examines the newest 10,000 retained messages per
conversation. The account's read position is saved on the server; the open inbox
refreshes periodically. Session changes retire pending history work and clear
its dialog data.

Moderators, admins and owners can choose **Remove** on public messages, including
older messages in Room history. Confirmation is required. Removal leaves a
**Message removed** marker, clears reactions and quoted excerpts, and appears in
moderation history without copying the removed text. It also scrubs reconnect
replay and retry acknowledgements. PMs never offer moderator removal.

Camera, screen-share and audio-only tiles have identical 16:9 dimensions in
aligned rows, including the incomplete final row. Video fits inside each cell
without cropping; portrait content and rotation do not change the cell size.
Crowded layouts scroll from the top. Pinning moves a tile first and highlights
it without enlarging it or reserving a separate row. A compact viewing-controls
button opens each remote tile's actions,
including Pin, volume and fullscreen, without covering the tile with separate
permanent buttons.

## Availability, request ownership and large rooms

Account lists signed-in sessions with the current one marked, sign-in and
last-refresh times, and expiry. A session is a browser sign-in shared by its tabs;
last refresh is not a measurement of user activity. Sign out individual sessions
or all other sessions while keeping this one. These actions share ownership with
profile, password and passkey changes; late responses cannot affect a replacement
account. Passkey-only accounts retain sessions, registration invitations, and
the separate **Sign out and forget this device** action.

On startup the client reads the versioned `/api/capabilities` response and shows
the account, registration and directory actions the server supports. Invite-only
password registration explains the required code; passkey registration appears
only when the server supports that separate flow. Capabilities are public UI
hints, not authorization. Discovery is required before account actions, directory
requests, invitation previews or room entry. A failed or invalid response keeps
those actions unavailable and shows an explicit retry; each attempt has a
15-second deadline and overlapping attempts are suppressed. After recovery,
only the returned features are enabled, including guest rooms when advertised.
Reload after changing server capabilities.

The named APIs in `ui.ts` bound the complete response to 15 seconds, including
JSON decoding, and propagate caller cancellation. Directory searches cancel the
previous request. Authentication and WebSocket operations retain their separate
ownership and deadline policies. A cancelled, malformed or unconfirmed mutation
may already have committed on the server: its submitting button stays disabled,
and the message asks the user to reload and inspect the result before trying
again. Definite HTTP rejections such as validation and permission failures permit
a corrected retry. There is no automatic mutation replay. Closing room creation
or selecting a newer destination prevents its late response from joining or
replacing the selection; a successful detached creation remains in My rooms.

Sign-in, registration and room creation use native modal dialogs so keyboard
focus stays within the active dialog and returns when it closes. Local storage
is optional for display name, layout and microphone preference: blocked access
or quota failures fall back to tab memory. These fallbacks do not make browser
storage durable and do not replace account/session security handling.

The roster reconciles keyed rows only in the visible People panel, retaining
unchanged controls and keyboard focus as membership changes. Profile reads share
in-flight promises, run at most eight requests concurrently, and retain at most
512 current members. Members beyond that bound keep their initials; explicitly
opening a profile takes the next available read slot before queued avatar work.
Leaving the room or changing identity retires
the cache. My rooms loads memberships in cursor pages with an explicit Load more
button. `GET /api/rooms/memberships` always returns `{items, next_cursor}`;
subsequent pages supply the returned cursor as `?after=...`.

Media subscription remains an explicit scaling limit. The browser requests each
remote producer; offscreen tiles do not automatically release or pause their
subscriptions. Tile size caps a camera's simulcast layer but does not remove its
consumer. The viewer's Hide action pauses the relevant remote video locally and
on the server, retaining its consumer slot for a quick resume. The default
`MAX_CONSUMERS_PER_PARTICIPANT=64` therefore fits at most 32 remote publishers
with microphone and camera, or 16 with microphone, camera, screen video and
screen audio, before additional subscriptions can be refused. These are track
counts, not tested capacity guarantees; mixed publication changes the totals.

A webinar with a few publishers and many viewers has different browser and SFU
costs from a call where everyone publishes. Increasing the consumer limit alone
does not establish capacity. A future large-call policy should explicitly bound
subscribed video, prioritize pinned/shared/active-speaker tracks, release unused
consumers and coordinate audio choices with the server. Such a policy requires
decoding, focus, recovery and bandwidth tests; it is not enabled by this client.
Use the [capacity and performance guide](../docs/performance.md) for measured
workloads and deployment limits.

## Tests

`npm --prefix web test` runs Node source-level regressions with mocked browser
APIs. `npm --prefix web run typecheck` invokes the actual TypeScript 7 compiler
for browser source and Node/Vite tooling in separate ambient environments;
the source-test loader deliberately uses a separate TypeScript 6 compatibility
package with its compiler API. These are distinct validation paths. Native
type-aware Oxlint uses the same TypeScript project graph; JS test/harness files
receive syntax/correctness lint, not full fixture type checking.
Build scripts under `scripts/` use checked JavaScript with typed JSDoc and the
same strict Node-tooling configuration and type-aware lint rules as Vite.
The source suite separately compiles [type-contracts.ts](tests/type-contracts.ts)
with the installed production compiler. Its positive cases check inferred API
results; its expected errors reject mismatched actions, payloads and response
types. [Endpoint tests](tests/api-contracts.test.mjs) exercise real decoders with
mocked HTTP responses, including malformed JSON and no-content results.

[The browser guide](e2e/README.md) describes the pinned Playwright community suite
against a disposable database/server. It exercises built assets and real
Chromium decoding with fake capture devices; it also documents separate Firefox
and WebKit runs and their limitations. Run it for UI/account/media changes.

Manual checks remain necessary for branded Safari/Firefox, real permissions/devices,
screen sharing, keyboard/screen-reader access, mobile browsers, and picture-in-
picture/fullscreen support. See [testing](../docs/testing.md) and
[performance](../docs/performance.md).

Display names are mutable labels, not proof of identity. Rosters, message senders,
replies and PM targets display names without participant IDs or account/guest labels.
Your messages and quoted replies keep the name used when they were sent. Actions,
private conversations and moderation still target full server-issued participant
UUIDs internally; names never serve as lookup keys. Guests keep their ID through
reconnect, while account IDs are stable. Moderation confirmations retain identity
details to help identify the selected participant.

Hover or focus a name in the user list or chat to open a compact participant card;
clicking or tapping the name keeps it open. Cards label the room nickname and account
name separately, with an optional public avatar/bio and relevant message/profile actions. Tab enters the card actions,
Escape dismisses it, and touch users can tap outside to close it. Profile responses
belong to the current card and room membership, so leaving or changing users cannot
populate a later card. The existing actions menu remains available through More
and the roster's actions button.

Account settings save profile-card color and treatment separately from chat styling.
Room owners can choose independent styles for the room name and header description
(the topic) through My rooms → Edit room or Manage room → Room appearance. The
palette and accent/text/tinted-background choices have live previews. Saved room
styles update the heading for current participants and survive reconnects. These
public styles accept palette tokens, not arbitrary CSS.

Chat timestamps are visible by default; an explicitly saved timestamp preference
is retained. Join and leave notices include timestamps, including departures by
people without a camera or microphone. Reconnect cleanup does not announce false
departures. The highlighted ellipsis opens Chat options.

Ordinary sign-out revokes the session before clearing saved app data from this
browser and reloading. A failed revocation preserves the active session; a late
completion cannot clear a replacement identity. Blocked storage cleanup is reported.
Desktop notifications are opt-in and generic by default. Sender/message previews
require a separate local preference and can expose content on the OS lock screen.
Room/account teardown closes outstanding notices and retires their click handlers.
Account password selection counts 15–128 Unicode scalar characters after NFC
normalization, with a 512-byte raw UTF-8 limit; confirmation uses the same normalization.

Authenticated WebSocket connections first mint a one-use upgrade ticket through
`POST /api/auth/ws-ticket` with the access token in the ordinary HTTP Authorization
header. The handshake carries only `simplestchat` and `ticket.<ticket>` protocols;
it never carries the reusable JWT. Tickets expire within 30 seconds, and every
reconnect mints a fresh one. Client preparation has a 15-second deadline within the
existing reconnect budget. Replacing the account/token, disconnecting or exhausting
recovery cancels the owned preparation; late responses cannot open another identity's
socket. Guests connect without a ticket. Authentication renewal on an established
socket remains a correlated WebSocket frame, with its existing bounded retry policy.
