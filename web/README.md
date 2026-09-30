# Browser client

TypeScript/Vite UI using mediasoup-client. Commands below run from the repository
root. The manifest specifies the minimum Node version; CI pins its exact Node
release. Dependencies and development tools resolve through the lockfile.

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
| `src/ui.ts` | Text-safe controls, dialogs and named HTTP API methods |
| `src/auth.ts`, `account-session-sync.ts`, `community-ui.ts` | Identity/session refresh and cross-tab reconciliation, profiles, recovery, room management |
| `src/signaling.ts`, `protocol.ts` | WebSocket lifecycle and typed server protocol |
| `src/validation.ts`, `api-validation.ts`, `protocol-validation.ts` | Shared decoder primitives and HTTP/WebSocket response validation |
| `src/room.ts`, `room-navigation.ts` | Membership, confirmed room controls, lobby status and explicit URL navigation |
| `src/media.ts` | Transports, capture, producers and consumers |
| `src/media-controls.ts`, `audio-output.ts` | Private device preview, live hardware lists, speaker selection/test and viewer-local playback |
| `src/layer-cap.ts` | Simulcast layer a remote tile can use at its rendered size, with hysteresis |
| `src/settings-dialog.ts` | Shared settings tabs and native-dialog dismissal |
| `src/social-chat.ts`, `chat-store.ts` | Room/PM conversations, composer, bounded replay and preferences |
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
Device IDs remain tab-local; browsers without output routing use the system
output. Speaker testing plays a short local tone and releases its URL/timer on
completion or dialog departure. Device-list changes never request capture.
Screen sharing distinguishes cancellation/permission failure from setup errors,
and reports whether optional screen audio is included, unavailable or ended.

## Availability, request ownership and large rooms

On startup the client reads the versioned `/api/capabilities` response and shows
the account, registration and directory actions the server supports. Invite-only
password registration explains the required code; passkey registration appears
only when the server supports that separate flow. Capabilities are public UI
hints, not authorization. If an older server does not implement the endpoint,
the client preserves its existing entry points and displays endpoint errors when
an unavailable action is attempted. Reload after changing server capabilities.

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
button; older servers' array responses remain accepted as a single page.

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
replies and PM targets show a discriminator derived from the server-issued participant
UUID; full IDs are available through accessible sender labels and hover details.
An account badge indicates the current participant authenticated to this server,
not a verified real-world identity. Guests keep their ID through reconnect, while
account IDs are stable. Offline message identities remain distinguishable by ID.

Account password selection counts 15–128 Unicode scalar characters after NFC
normalization, with a 512-byte raw UTF-8 limit; confirmation uses the same normalization.
