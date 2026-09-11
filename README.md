# SimplestChat

A Rust and mediasoup WebRTC SFU for audio/video rooms, with a TypeScript browser
client and optional PostgreSQL-backed accounts and communities.

## What it does

- Camera, microphone, push-to-talk, screen sharing, simulcast and active speakers.
  Private device preview and personal playback controls keep capture intentional.
- Public room directory, owned rooms, rules/images, passwords, lobby admission,
  roles, bans, moderation and voice requests.
- Room chat and private conversations, delivery state, unread counts, drafts,
  ignore/preferences and bounded reconnect replay.
- Password/passkey accounts, editable profiles, password changes and one-time
  saved recovery keys. Guest rooms can run without PostgreSQL.
- Responsive room UI with remembered desktop panels and mobile participant actions.

Shared-media queues/watch sessions, durable chat history, recording and email
delivery/verification are not implemented. A saved recovery key is not an
email-reset flow. See [limitations and security boundaries](docs/deployment.md#limitations-and-operational-caveats).

## Try the web UI locally

Prerequisites: rustup, a native C++ build environment and Node/npm. Follow the
[development setup](docs/development.md) first for native prerequisites.

From the repository root:

```sh
build/run-local.sh
```

The launcher selects the pinned Rust compiler even when Homebrew's Rust comes
first, installs missing static OpenSSL, restores locked web dependencies, builds
the UI and runs the debug server. On macOS it detects the active interface's LAN
IPv4 address; elsewhere supply `ANNOUNCE_IP` explicitly. Use `--skip-web` to reuse
built assets for a faster restart. HTTP stays on loopback with one media worker.

Open `http://localhost:3000` in two browser contexts and join the same room.
Joining does not capture or publish; use **Cam** or **Mic setup** explicitly.
Advertise a reachable LAN address for manual browser media testing, even when
loading the page from localhost.

This starts guest-only mode unless local database/auth settings are already present
in your environment. Accounts and persistent rooms need a dedicated PostgreSQL
database and JWT secret; see [full local UI setup](docs/development.md#accounts-and-persistent-rooms).
The launcher enables ad-hoc rooms; registration and startup migrations remain opt-in.

For live web development, see [web/README.md](web/README.md). The Rust server serves
`web/dist`, so rebuild assets after changes unless using Vite's development server.

## Project guide

| Area | Start here |
| --- | --- |
| Native build, Mac setup, local launch | [Development](docs/development.md) |
| Frontend modules and development server | [Web client](web/README.md) |
| Server architecture and protocol | [Rust server](src/README.md) |
| Environment variables and limits | [Configuration](docs/configuration.md) |
| Production, migrations, TLS and security | [Deployment](docs/deployment.md) |
| Unit, database, browser and container checks | [Testing](docs/testing.md) |
| Load testing and regression measurement | [Performance](docs/performance.md) |
| Native dependency provenance and update gates | [Vendor notes](vendor/README.md) |

Version pins live in [Cargo.toml](Cargo.toml), [rust-toolchain.toml](rust-toolchain.toml),
[web/package.json](web/package.json) and their lockfiles. Production builds omit
the optional synthetic load-client dependency graph.

## Validation

After configuring the native environment and installing web dependencies:

```sh
cargo fmt --all -- --check
cargo test --locked --all-features -- --test-threads=1
npm --prefix web test
npm --prefix web run build
```

The ordinary Rust command skips three PostgreSQL integration tests. The
[testing guide](docs/testing.md) explains how to run them and the real-browser
community suite against disposable local services.

Historical load numbers are **not a current capacity guarantee**. Synthetic RTP
measures transport/forwarding, not browser video quality. See the
[measurement protocol](docs/performance.md) and [recorded results](docs/performance-results.md)
for evidence, limitations and remaining validation.

## Before exposing a server

Read the [deployment guide](docs/deployment.md). In particular, keep the backend
behind TLS, configure exact origins and independent secrets, migrate with a
separate role, and verify remote database TLS. One application replica is the
supported topology; room state, WebAuthn challenges and rate limits are local.

Private messages and SFU media are not application-layer end-to-end encrypted.
Secret rooms are unlisted, not access-controlled merely by being hidden.
