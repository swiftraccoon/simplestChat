# Development

Run commands from the repository root.

## Prerequisites

- Rust via rustup; the version is pinned in [rust-toolchain.toml](../rust-toolchain.toml).
- Node.js and npm (minimum Node 22.12).
- Xcode command-line tools on macOS, or a Linux C++ toolchain.
- `make`, `perl`, `curl`, `pkg-config`, `cmake`, and Python 3/pip.
  See [native dependencies](../vendor/README.md) for platform details.
- PostgreSQL if you want accounts and persistent rooms.
- ShellCheck for the helper-script quality gate (`brew install shellcheck` on
  macOS, or your Linux package manager).
- `jq` for container benchmark validation tests (`brew install jq` on macOS).

## Guest-only local UI

```sh
build/run-local.sh
```

Open `http://localhost:3000`. The launcher selects the pinned Rust toolchain,
installs missing static OpenSSL, restores web dependencies, builds the UI, and
runs the server. The first native build can take several minutes. Stop it with
Ctrl-C.

To restart without reinstalling or rebuilding the web client:

```sh
build/run-local.sh --skip-web
```

Use the normal command after web changes. For hot reload, see
[frontend development](../web/README.md#build-and-develop).

HTTP listens on `127.0.0.1:3000`, with one media worker on UDP 40000.
On macOS the launcher detects the default network interface, falling back to
`en0`. Set `ANNOUNCE_IP` to a reachable LAN IPv4 address if a VPN or another
interface makes that choice unsuitable; it is required on Linux.
Browser media can need a LAN address even when the page is on localhost.

To run a second instance, choose unused HTTP and media ports:

```sh
PORT=3109 WEBRTC_SERVER_PORT_BASE=41000 build/run-local.sh --skip-web
```

Joining a room does not start capture. Open **Your settings** or **Mic setup**
for a private device preview. Use the camera or microphone button to broadcast.

## Accounts and persistent rooms

Create a local development database, then configure:

```sh
export DATABASE_URL='postgres://YOUR_USER:YOUR_PASSWORD@127.0.0.1:5432/simplestchat_dev?sslmode=disable'
export JWT_SECRET="$(openssl rand -hex 32)"
export RUN_MIGRATIONS=true
export REGISTRATION_ENABLED=true
build/run-local.sh
```

Startup migrations change the database schema, so use a dedicated development
database. The role needs permission to install the `pg_trgm` extension.
Keep credentials out of Git and reuse the JWT secret across restarts to preserve
sessions. The launcher accepts only loopback database URLs; `sslmode=disable`
is for local development.

Without `DATABASE_URL`, the server is guest-only. Accounts also require a JWT
secret of at least 32 bytes. Registration and startup migrations default off.

For passkeys, set `WEBAUTHN_RP_ID=localhost` and
`WEBAUTHN_ORIGIN=http://localhost:3000`. The origin must match the page you open,
including its port when using Vite or a second instance.

## Native and web build

For release builds or direct Cargo commands, configure the native environment:

```sh
build/install-openssl.sh "$PWD/target/openssl-3.5.8"
export OPENSSL_DIR="$PWD/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$PWD/build/pip-constraints.txt"

npm ci --prefix web
npm --prefix web run build
cargo build --locked --release --bin simplestChat
```

The OpenSSL helper installs the checksum-pinned static build. Python build tools
use the constraints file. Run the server from the repository root so it can
find `web/dist` and migrations; reload Rust Analyzer after the first native setup.

### Rust toolchain troubleshooting

If direct Cargo commands use Homebrew Rust instead of the pinned rustup toolchain:

```sh
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
rustc --version
cargo --version
```

If rustup proxies are unavailable, select the compiler explicitly:

```sh
export RUSTC="$(rustup which --toolchain 1.98.1 rustc)"
export RUSTDOC="$(rustup which --toolchain 1.98.1 rustdoc)"
rustup run 1.98.1 cargo build --locked --release --bin simplestChat
```

Keep the release `build-override` in Cargo.toml: macOS needs it to load
proc-macro libraries.

## Passwords and recovery

In **Account**, generate a recovery key using your current password and save it.
The key is shown once; generating another replaces it. **Recover with a saved
key** consumes the key and sets a new password. Password changes and recovery
sign out existing account sessions.

Passwords must be 8–128 UTF-8 bytes, match confirmation, and contain no control
characters. Passkey-only accounts cannot use current-password controls.
Email ownership is not verified, and there is no password-reset email flow.

## Further reading

- [Testing](testing.md): unit, database, browser, and container checks.
- [Configuration](configuration.md): environment variables and defaults.
- [Performance](performance.md): load tests and comparisons.
- [Deployment](deployment.md): production setup.
