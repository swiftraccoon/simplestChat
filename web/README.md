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

The homepage and account surfaces load the application shell without the call engine.
`room.ts` imports `MediaManager` dynamically only during explicit room setup;
`capture-preferences.ts` contains the lightweight settings and capture helpers shared
with the private preview UI. Opening account settings, Messages or a contact offer
neither downloads mediasoup nor captures a device. A failed call-engine download
keeps room chat connected. **Refresh incoming media** makes an explicit load attempt without starting a
microphone, camera or screen share. Browsers can retain a failed module import;
in that case the user must leave and reload before joining again. Managed releases
retain a bounded set of fingerprinted assets through the proxy so deferred imports
can survive upgrades and rollbacks. Retention has an age and generation limit;
it does not promise indefinite compatibility between old clients and a new server.
An available update is an explicit action outside a call; it never reloads a call.

**Refresh incoming media** replaces only the receive transport and selected subscriptions.
It leaves the sender, capture tracks, membership and viewing choices intact. Native
consumer setup has a ten-second deadline; an expired operation retires its receiver
and cannot block the replacement's voice-first negotiation queue. **Call health**
shows separate sending, receiving and playback evidence, even with reliability
sharing off. Unsupported or stale statistics stay unknown, and quiet audio or a
still picture alone cannot establish a failure.

Account destinations use `#view=inbox`,
`#view=conversation&peer=<account UUID>` and
`#view=settings&section=<section ID>&segment=you|room`. A selected room is retained
as `room=<room ID>` alongside the view; existing `#<room ID>` links still select a
room. `app-navigation.ts` owns surface history and identity-scoped restoration;
`room-navigation.ts` owns explicit room selection and departure. Browser Back pops
nested dialogs or settings screens before leaving the call. Forward restores
read surfaces; reload restores a supported section or conversation only after
account restoration, retaining its saved composer draft without sending it.
Contact and invitation fragments work on initial load and on arrival in an open
app, and retain their explicit acceptance steps. Temporary confirmations are
closed on Back and never replay an action on Forward.

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

| Module                                                                                          | Responsibility                                                                                                                            |
| ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `src/main.ts`                                                                                   | Application wiring, room shell and layout                                                                                                 |
| `src/video-layout.ts`                                                                           | Uniform media cells sized to the available stage                                                                                          |
| `src/ui.ts`                                                                                     | Text-safe controls, dialogs and named HTTP API methods                                                                                    |
| `src/participant-hovercard.ts`                                                                  | Shared participant cards for roster and chat names, profile loading ownership and pointer/keyboard/touch interaction                      |
| `src/appearance.ts`                                                                             | Independent profile-card and room-heading appearance controls, previews and palette-only rendering                                        |
| `src/auth.ts`, `account-session-sync.ts`, `community-ui.ts`                                     | Identity/session refresh and cross-tab reconciliation, profiles, recovery, room management                                                |
| `src/account-security.ts`, `account-sessions.ts`                                                | Shared account-mutation ownership, passkey/recovery management and signed-in session controls                                             |
| `src/account-preferences.ts`                                                                    | Account-owned privacy state, field PATCH intents, cross-device refresh and uncertain-save reconciliation, shared by home and room screens |
| `src/account-updates.ts`                                                                        | Authenticated account invalidations, bounded SSE decoding, visibility ownership and reconnect repair                                      |
| `src/pwa.ts`, `public/sw.js`, `public/manifest.webmanifest`                                     | Installation, optional generic PM push alerts and account-scoped subscription ownership; no offline cache                                 |
| `src/signaling.ts`, `protocol.ts`, `protocol.generated.ts`                                      | WebSocket lifecycle and the generated server protocol                                                                               |
| `src/validation.ts`, `api-validation.ts`, `protocol-validation.ts`                              | Shared decoder primitives and HTTP/WebSocket response validation                                                                          |
| `src/room.ts`, `room-navigation.ts`                                                             | Membership, confirmed room controls, lobby status and explicit room selection/departure                                                   |
| `src/app-navigation.ts`                                                                         | Typed account destinations, identity-scoped surface history and Back/Forward restoration                                                  |
| `src/room-administration.ts`, `room-administration-api.ts`, `room-administration-validation.ts` | Shared room-management screens, authenticated HTTP controls and response validation outside room membership                               |
| `src/media.ts`                                                                                  | Deferred call engine: transports, capture, producers and consumers                                                                        |
| `src/call-health.ts`, `src/call-health-ui.ts`                                                   | Bounded local media evidence and explicit playback, device and receive recovery actions                                                   |
| `src/app-updates.ts`                                                                            | Public revision hint with an explicit reload outside calls                                                                                |
| `src/capture-preferences.ts`                                                                    | Lightweight capture types, local settings and explicit preview capture helpers                                                            |
| `src/screen-presentation.ts`                                                                    | Details/Motion preference and presentation constraints/encoding policy                                                                    |
| `src/media-lifecycle.ts`                                                                        | Coalesced visibility, page-resume, online and hardware hints without new capture or room entry                                            |
| `src/media-controls.ts`, `audio-output.ts`                                                      | Private device preview, live hardware lists, speaker selection/test and viewer-local playback                                             |
| `src/receive-policy.ts`                                                                         | Bounded incoming video selection, optional stable speaker following, visible/pinned priority and data saver/audio-only preferences        |
| `src/attachments.ts`                                                                            | Bounded paste/drop/picker uploads, progress/retry and gesture-opened previews/downloads                                                   |
| `src/notification-preferences.ts`, `notification-validation.ts`                                 | Account-synced notification rules, conversation mute/snooze and quiet hours                                                               |
| `src/layer-cap.ts`                                                                              | Simulcast layer a remote tile can use at its rendered size, with hysteresis                                                               |
| `src/settings-sheet.ts`                                                                         | The settings sheet: one list, single-purpose screens, rows that apply at once                                                             |
| `src/social-chat.ts`, `chat/*.ts`, `chat-store.ts`                                                           | Room/PM conversations, composer, bounded replay, removal tombstones and preferences                                                       |
| `src/chat-history.ts`                                                                           | Saved room history, retention controls, account PM inbox, search and shared read positions                                                |
| `src/pm-composer.ts`, `pm-send-attempts.ts`, `chat-reactions.ts`                                | Durable account-PM attempts and quoted replies, plus shared reaction controls                                                             |
| `src/discovery.ts`, `discovery-validation.ts`                                                   | Accepted contacts, explicit contact links, account favorites and recent room shortcuts                                                    |
| `src/*.css`, `index.html`                                                                       | Layout, component styles and initial document                                                                                             |
| `public/help.html`, `help.css`                                                                  | Zero-JavaScript user help, copied into the production output                                                                              |
| `scripts/check-bundle.mjs`, `bundle-budget.json`                                                | Aggregate raw/gzip asset limits enforced by every production build                                                                        |

`social-chat.ts` coordinates room/account membership, history, replay and read
positions. The `chat/` modules own the store boundary, rendering, composer,
mentions, notifications and preferences. They receive narrow live contexts;
`dispose()` releases subscriptions, observers and listeners, while activation
identity fences work already awaiting a result. `disposable.ts` provides
shared listener signals and resource cleanup. Chat tests import the actual
modules through `tests/native-fixtures.mjs`.

Rust signaling types in [src/signaling/protocol.rs](../src/signaling/protocol.rs)
are the wire contract's source of truth: `cargo run -q -p xtask -- protocol-ts`
writes `src/protocol.generated.ts` from them (`--check` fails when it is stale),
and `src/protocol.ts` re-exports those types beside the hand-written helpers.
Change the Rust side, regenerate, then update the decoders and handlers.
See the [protocol guide](../docs/protocol.md) for sequencing, correlation limits,
reconnect/replay and runtime validation.

Use the named `api` methods for profile, recovery and room HTTP requests; the
method owns the route, request body and result decoder. Social requests derive
their payload and response from the action instead of accepting a caller-selected
result type. Both boundaries start with `unknown`, strip unrecognized fields and
preserve the contract's nullable values. Add a decoder and valid/invalid tests
when extending either boundary, not an assertion at the UI callsite.

Joining does not capture or publish. Preview is local until explicit publication.
Settings is one non-modal sheet (`settings-sheet.ts`) hosted in the room's side
column, docked beside the home page, or full-screen on phones. Its root list has a
You segment (Camera & mic, Speaker, Layout, Chat look, Notifications, Privacy,
Profile, Sign-in & security, Invites) and, for owners and admins, a This room segment
(Who can enter, Chat & broadcasting, Name & topic, People, Bans, Reports, History,
Invitations). Each row opens one screen; every control applies as it changes and
shows a brief Saved mark, so only multi-field forms keep a button (Save profile,
Change password, Create invitation). Escape pops a screen and closes at the root;
closing returns focus to the opener, or to the control that was focused when the
sheet opened if the opener now sits in a closed menu. Leaving a screen stops its
preview, test tone and device observer; a changed identity closes the sheet even
while a screen is busy. Viewer mute/volume/hide must not change what anybody else
receives. Async media and account work can outlive a screen or session; stale
completion must not attach tracks or account data to a replacement session.

Room links and browser history select a room; joining from them remains explicit,
while a directory tile joins directly once a name is known. Invitations
use 32-symbol secrets in fragments (`#invite=` for rooms, `#register-invite=` for
registration), scrubbed before room navigation. Room invitations require signed-in
preview and explicit acceptance through JSON request bodies. Acceptance selects
the room; joining is a separate action. Management lists and revocation use nonsecret
invitation IDs; a newly created secret is shown only once. Account-room Join buttons
retain their explicit join intent until signaling and any departure finish. A newer
destination, edited room ID or account change retires asynchronous invitation work
and pending joins.
Home, Leave and lobby Cancel clear the selected room URL. No URL change starts capture.
Room settings on the sheet's This room screens apply over the room socket one
change at a time: the screen is busy until the server confirms, a failed change
keeps its draft and offers Retry change, and a completion that arrives after a
newer attempt or a different room cannot unlock the screen. Name, topic,
description, image and appearances save over HTTP as each field changes and
reach current participants at once. A connected lobby moderator is availability
information, not a promise of admission.

Output selection is feature-detected and applies to owned playback elements.
Output device selection remains tab-local; saved camera/microphone IDs and capture
preferences use browser-local storage. Browsers without output routing use the
system output. Speaker testing plays a short local tone and releases its URL/timer on
completion or leaving the Speaker screen. Device-list changes never request capture.
The camera and microphone controls request access directly when explicitly
activated; opening Settings or joining a room does not capture. Camera & mic offers
separate private **Test camera** and **Test microphone** actions beside the
selectors to grant access and refresh device names. Choosing a device applies at
once: it switches an active camera or microphone (reverting the selector if the
switch fails) but never turns an inactive device on. Resolution, frame rate and
audio processing sit behind the Quality row.
Camera publication, recapture and preview share `captureMedia`: a
`NotReadableError` with requested resolution/frame-rate preferences permits one
retry without those quality constraints. The retry preserves an exact selected
camera and any audio constraints. Permission denial and other errors do not
trigger it; cancelled work cannot begin a retry, and late streams are stopped.
Screen sharing distinguishes cancellation/permission failure from setup errors,
and reports whether optional screen audio is included, unavailable or ended.

The visual system is one warm black ground (`--bg`) with surfaces that step up in
the same hue and no accent hue: links and the primary action use the ink itself,
and status colors (live, danger, warning) are the only saturated values. Panels
are separated by tone, never by a line or shadow. Text is Outfit, self-hosted from
`public/fonts/` (SIL OFL 1.1, latin subset) because the content security policy
allows no other font source; the file is outside the bundle budget. The room uses
one header: on a desktop it becomes the top of the people column (wordmark, room
name, topic) while the People/Chat/More tools float over the stage's top band, and
on phones it stays a bar. Call controls are bare circles under the video column so
the desktop People and chat panels retain their full height. Conversation buttons above chat
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
remain bounded room-session data. Start a conversation together in a room or use
**Messages → Contacts** to share/paste a contact link and accept a request. Accepted
contacts can start a PM without entering a room. Existing conversations can
continue while either person is offline, subject to PM opt-out and ignore preferences. Saved history pages hold
at most 50 messages, and search examines the newest 10,000 retained messages per
conversation. The account's read position is saved on the server. While the app is
visible, an authenticated account stream invalidates inbox, contact, privacy and
notification views even outside a room. Views read fresh HTTP snapshots after a
change or reconnection; periodic polling repairs an unavailable stream. Hidden
tabs close their streams and reconnect when visible. The Messages button shows
the account unread count.
Conversations resume around the first unread message; read positions advance only
while the conversation is visible and scrolled to the end of the loaded page.
Search results open with surrounding messages and older/newer navigation. Inbox
PM drafts persist on this device, separately for each account and recipient;
**Sign out and forget this device** clears them. Earlier retained public messages
load inside the existing chat scroll area; once the live buffer is full, the same
action opens the saved conversation at that older message. Session changes retire pending history
work and clear its dialog data.

A first HTTP account PM may also supply the current room as context. The server
requires both accounts to be connected and admitted to that exact room, confirms
the sender can chat, and checks both saved privacy policies before creating the
conversation. An arbitrary account ID or guest identity supplies no such proof.
Existing-conversation and accepted-contact sends need no room context; a confirmed
retry still returns its original receipt after departure.

Saved account PMs expose the same Reply, Edit and React actions from the room
conversation and the inbox. The server's account-conversation marker routes these
actions through the retained account API; guest PM actions remain bound to their
room session. Replies name an original message in the same conversation and
preserve that quote target in durable send recovery. Reaction requests express
whether one emoji should be present, so repeating a request does not toggle it
back. Earlier retained public messages use the saved room history authority for
Reply and React instead of requiring the message to remain in membership replay.

Authors can edit their messages from live chat or saved history. An **edited**
indicator records changes; a revision check prevents an older open editor from
overwriting a newer edit. Loaded quotes update and removed content cannot return
through delayed acknowledgements. Moderators, admins and owners can pin up to
three public messages. The compact **Pinned** control expands within the chat;
private messages cannot be pinned. Saved pins expire with their messages.

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

The home page is a directory. The title bar carries the Rooms title, its order
and the search beside the wordmark, with Create Room for accounts at its right
beside Help and the account state: Sign in for guests, or an avatar-and-name
button that opens Account, Messages (with its unread count), My rooms and Sign
out. Inside a room those items sit flat in the More menu, Sign out is not
offered, and the directory toolbar is hidden. Below the title bar a grid of
portrait room tiles fills the page's width on the header's own gutter, and a
dock at the bottom edge holds the join bar and the page's tools. The dock stays
at the bottom whether the grid is short or scrolls beneath it; on phones it
follows the grid. Tiles are ordered busiest first by the
server: live rooms by participant count (unknown counts after known ones), then
rooms nobody is in, newest first, across Load more pages and searches. A tile
shows the room image or a monogram of its name on a tone chosen from the room id;
on desktops tiles are about 1.5 to 1.8 times a phone's two-up tiles, snapping to
however many columns fit. An account's favorite and recent rooms, favorites
first, stand in a rail of small square tiles at the left of the grid, each with
its star; the rail collapses to a strip at the left edge on its toggle (the
choice is kept on this device), lies across the top on phones, and is absent
while nothing is saved. Clicking a tile, in the rail or in the grid, enters
its room when a name is set (an account name, or one typed into the join bar);
without a name it selects the room and asks for one first. The join bar itself
serves rooms that are not listed.

Favorite rooms and successful recent room joins are saved with the account and
appear on the home page shelf and in **My rooms**. Star a room to keep it among
up to 100 favorites; another 50 recent rooms are retained. Switching devices
restores these shortcuts without automatically joining. Unlisted room details
remain visible only to current owners or members. Contact requests and favorites
use the same account fencing and explicit actions as the rest of the account UI.

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
(the topic) on the sheet's Name & topic screen, reached from My rooms → Edit room
or from Manage room inside the room. The
palette and accent/text/tinted-background choices have live previews. Saved room
styles update the heading for current participants and survive reconnects. These
public styles accept palette tokens, not arbitrary CSS.

My rooms also opens shared management screens for a persisted room without
joining it. Authorized owners, admins and moderators can inspect people, bans,
reports, moderation history and the live waiting room; available actions follow
their current role. Kick/ban and camera/chat restrictions require a currently
admitted target. Admit/Deny works while the operator stays home or in another
call. Restoring permission never starts the target's capture. Lists use bounded
pages, and an uncertain mutation asks for explicit state refresh before retry.

Chat timestamps are visible by default; an explicitly saved timestamp preference
is retained. Join and leave notices include timestamps, including departures by
people without a camera or microphone. Reconnect cleanup does not announce false
departures. The highlighted ellipsis opens Chat options.

Account privacy is available from the signed-in home page, saved conversations
and room settings. `AccountPreferences` owns it independently of a room. Requests
carry only explicit field changes or ignore/unignore set operations; the server
merges them with current state under account/session locks. Concurrent appearance
changes therefore preserve a newer ignore or private-message opt-out. Healthy
account streams replace periodic preference polling once the view has reconciled;
focus checks and polling repair disconnected or unreconciled views. Cross-tab
change hints contain no preference data or credentials.

An uncertain save preserves restrictive local intent and disables further changes.
**Refresh privacy** explicitly reads current account state and synchronizes active
room policy before enabling changes again; the client never automatically replays
the mutation. Delayed reads and writes belong to their original account and cannot
populate a replacement identity. A failed read leaves existing choices intact.
Server room policy comes from PostgreSQL, including direct join and lobby
admission, and a stale client command cannot re-enable saved account opt-outs.
Guest ignores remain specific to their room membership.

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

## Screen sharing

**Your settings → Screen sharing** offers Details (the default, up to 15 fps) and
Motion (up to 30 fps), both bounded to 1920×1080. The choice is saved on this
device and applies the next time **Share screen** is chosen. Details favors
readable slides and text. Supported layered encoders adapt to available bandwidth;
the negotiated codec and actual encoder capabilities remain authoritative.
While sharing, the camera uses its two lower layers when available to leave room
for the presentation and voice; stopping or failing to start the share restores its
normal layer budget without starting a device. Small tiles do not lower screen
share quality, while explicit receive limits such as Data saver still apply.

## Incoming media

Receive settings offer Balanced (up to nine incoming videos), Data saver (up to
four, lowest simulcast layer) and Audio only. Camera and screen-share tiles retain
identical dimensions. Picture-in-picture and explicit pins take priority, then
observed visibility. Among equally visible tiles, screen shares receive before
cameras; existing subscriptions break remaining ties. Offscreen videos are
unsubscribed, and a hidden tab keeps only an explicit picture-in-picture video.
Closing a consumer is acknowledged before a replacement is opened. Audio
reception and publishing controls are independent. The receive choice is saved
on this device.

**Follow speaker** is an optional choice in the room's More menu and starts off.
An available remote camera gains priority after its speaker remains stable for
1.5 seconds; established focus holds for at least five seconds before another
speaker can replace it. Pins, picture-in-picture and visible shared screens keep
priority. A focused camera can enter the receive set from outside the viewport,
and its tile moves forward without changing any cell's dimensions. Hiding a
person, Audio only and page visibility remain authoritative. Turning the choice
off, losing signaling, or retiring its camera/membership cancels pending focus;
the preference survives reload, while reconnect waits for fresh speaker signals.

## Attachments

Signed-in people can attach up to four files, each up to 5 MiB, by pasting,
dropping or using the file picker in live chat and saved PM conversations.
Uploads show progress, support retry/removal and must finish before sending.
Switching conversation or account cancels pending selections; sent files travel
with the message's existing delivery/retry identity. Names and sizes render as
text. Raster previews and downloads start only after a click; access credentials
stay in request headers. Closing/replacing a message view aborts its requests and
revokes its object URLs. Across open views at most eight file blobs are retained.

## Notifications

Notification rules in Account follow the account across devices. Private-message
and mention switches, per-conversation mute/snooze, and named-time-zone quiet
hours apply to foreground notices/sounds and server push eligibility. Browser
notification permission remains an explicit choice on each device. Preferences
follow account-stream invalidations, with focus checks and a 30-second visible
polling fallback when the stream is unavailable or the view needs reconciliation.
Unknown or unreconciled policy suppresses alerts; a snapshot older than 90 seconds
also suppresses alerts while disconnected. Quiet-hour alerts are not replayed
afterward. Already accepted push delivery cannot be recalled.
