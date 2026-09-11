# Browser client

TypeScript/Vite UI using mediasoup-client. Commands below run from the repository
root. The manifest specifies the minimum Node version; CI pins its exact Node
release. Dependencies and development tools resolve through the lockfile.

## Build and develop

```sh
npm ci --prefix web
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
| `src/auth.ts`, `community-ui.ts` | Identity/session refresh, profiles, recovery, room management |
| `src/signaling.ts`, `protocol.ts` | WebSocket lifecycle and typed server protocol |
| `src/validation.ts`, `api-validation.ts`, `protocol-validation.ts` | Shared decoder primitives and HTTP/WebSocket response validation |
| `src/room.ts` | Membership, lobby, moderation and room events |
| `src/media.ts` | Transports, capture, producers and consumers |
| `src/media-controls.ts` | Private device preview and viewer-local playback controls |
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
