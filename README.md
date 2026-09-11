# SimplestChat

Audio/video chat rooms built with Rust, mediasoup, and a TypeScript web client.
Run guest rooms on their own, or add PostgreSQL for accounts and persistent rooms.

## Features

- Camera, microphone, push-to-talk, screen sharing, and private device preview.
- Public room directory, owned rooms, passwords, lobby admission, and moderation.
- Room chat and private messages, unread counts, drafts, and ignore controls.
- Password/passkey accounts, profiles, and saved recovery keys.
- Responsive layout and per-participant volume, mute, and video controls.

Shared watch sessions, recording, durable chat history, and email verification
are not implemented.

## Run locally

Install rustup, Node/npm, and the [native build prerequisites](docs/development.md#prerequisites).
From the repository root:

```sh
build/run-local.sh
```

Open `http://localhost:3000` in two browser windows and join the same room.
Enable your camera or microphone when ready; joining does not start capture.

The launcher installs missing OpenSSL, builds the web client, and runs the server
with the pinned Rust toolchain. The first build can take several minutes.
Use `build/run-local.sh --skip-web` for faster restarts when web files are unchanged.
On macOS, the media address is detected automatically; elsewhere set `ANNOUNCE_IP`
to your machine's LAN IPv4 address.

Without database settings, the server runs guest-only rooms. For accounts and
persistent rooms, follow [local database setup](docs/development.md#accounts-and-persistent-rooms).
For frontend hot reload, see [the web guide](web/README.md).

## Project guide

| Topic | Guide |
| --- | --- |
| Build, local launch, and troubleshooting | [Development](docs/development.md) |
| Frontend development and modules | [Web client](web/README.md) |
| Server architecture and protocol | [Rust server](src/README.md) |
| Environment variables and limits | [Configuration](docs/configuration.md) |
| Production setup and security | [Deployment](docs/deployment.md) |
| Unit and integration tests | [Testing](docs/testing.md) |
| Benchmarks and measurements | [Performance](docs/performance.md) · [Results](docs/performance-results.md) |
| Native dependency updates | [Vendor notes](vendor/README.md) |

## Checks

After [build setup](docs/development.md):

```sh
cargo fmt --all -- --check
cargo test --locked --all-features -- --test-threads=1
npm --prefix web test
npm --prefix web run build
```

Database and browser integration tests require a disposable local database;
see [testing](docs/testing.md).

## Deploying

Follow the [deployment guide](docs/deployment.md) for TLS, database migrations,
origins, and secrets. The server currently supports a single application replica.
Private messages and media are not end-to-end encrypted; secret rooms are
unlisted, not access-controlled. See [security limitations](docs/deployment.md#limitations-and-operational-caveats).
