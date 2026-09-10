# Browser client

TypeScript/Vite UI using mediasoup-client. Commands below run from the repository
root. Node/npm versions are pinned in the manifests and CI.

## Build and develop

```sh
npm ci --prefix web
npm --prefix web test
npm --prefix web run build
```

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
| `src/main.ts`, `ui.ts` | Application wiring, room shell, dialogs and layout |
| `src/auth.ts`, `community-ui.ts` | Identity/session refresh, profiles, recovery, room management |
| `src/signaling.ts`, `protocol.ts` | WebSocket lifecycle and typed server protocol |
| `src/room.ts` | Membership, lobby, moderation and room events |
| `src/media.ts` | Transports, capture, producers and consumers |
| `src/media-controls.ts` | Private device preview and viewer-local playback controls |
| `src/social-chat.ts`, `chat-store.ts` | Room/PM conversations, composer, bounded replay and preferences |
| `src/*.css`, `index.html` | Layout, component styles and initial document |

Rust signaling types in [src/signaling/protocol.rs](../src/signaling/protocol.rs)
are the other half of the wire contract. Update both sides together, including
camelCase room settings embedded in JSON values.

Joining does not capture or publish. Preview is local until explicit publication.
Viewer mute/volume/hide must not change what anybody else receives. Async media
and account work can outlive a dialog or session; stale completion must not attach
tracks or account data to a replacement session.

## Tests

`npm --prefix web test` runs Node source-level regressions with mocked browser
APIs. `npm --prefix web run typecheck` invokes the actual TypeScript 7 compiler;
the source-test loader deliberately uses a separate TypeScript 6 compatibility
package with its compiler API. These are distinct validation paths.

[The browser guide](e2e/README.md) describes the pinned Playwright community suite
against a disposable database/server. It exercises built assets and real
Chromium decoding with fake capture devices. Run it for UI/account/media changes.

Manual checks remain necessary for Safari/Firefox, real permissions/devices,
screen sharing, keyboard/screen-reader access, mobile browsers, and picture-in-
picture/fullscreen support. See [testing](../docs/testing.md) and
[performance](../docs/performance.md).
