# Development

Run commands from the repository root unless a section says otherwise.
The checked-in build tools support a clean clone; ignored `scripts/` are private
operator helpers and are not prerequisites.

## Prerequisites

- Rust 1.98.1 via rustup; [rust-toolchain.toml](../rust-toolchain.toml) pins the patch.
- Node.js 26.8.1 and npm (web minimum: 22.12).
- Current Xcode command-line tools on macOS, or a Linux C++ toolchain.
- `make`, `perl`, `curl`, `pkg-config`, `cmake`, Python 3/pip and the native
  dependencies described in [vendor/README.md](../vendor/README.md).
- Linux deployment builds also need the static C/C++ libraries used by
  [Dockerfile](../Dockerfile).
- PostgreSQL for accounts/persistence tests; it is optional for guest-only use.

The native build invokes Python tooling constrained by
[build/pip-constraints.txt](../build/pip-constraints.txt). Do not replace its
security-reviewed pins or static OpenSSL with arbitrary system dependencies.

## Native and web build

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

The OpenSSL helper verifies its pinned source checksum and installs static
OpenSSL 3.5.8 LTS under ignored `target/`. The native build requires at least
3.5.8 and below 3.6.0. The Rust Analyzer settings use the same prefix; reload the
editor after the first installation.

Check `rustc --version` and `cargo --version`. If Homebrew binaries precede
rustup's proxies, put the proxies first:

```sh
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
rustc --version
cargo --version
```

If that directory does not contain rustup proxies, explicitly select the compiler
for the current shell without changing global toolchain settings:

```sh
export RUSTC="$(rustup which --toolchain 1.98.1 rustc)"
export RUSTDOC="$(rustup which --toolchain 1.98.1 rustdoc)"
rustup run 1.98.1 cargo build --locked --release --bin simplestChat
```

Keep `[profile.release.build-override]` in Cargo.toml: macOS optimized/stripped
proc-macro dylibs otherwise fail to load. Native C++ errors are real build
failures; preserve complete logs and exit statuses.

## Guest-only local UI

After the build, on macOS:

```sh
ANNOUNCE_IP="$(ipconfig getifaddr en0)" MEDIA_WORKERS=1 ALLOW_AD_HOC_ROOMS=true ./target/release/simplestChat
```

Supply the active interface's LAN address if it is not `en0`. On Linux, supply
the appropriate client-reachable address explicitly. Open `http://localhost:3000`
in a normal and private browser window and join the same room.

A manual browser test should advertise a real LAN address, even on the same
machine. Loopback ICE candidates can fail despite successful signaling. The
isolated Chromium automation explicitly opts into loopback media; that does not
establish compatibility with Firefox, Safari or real devices.

Joining does not request camera/microphone access. Use **Cam** for initial setup,
or **Mic setup** / **Media setup** for private preview, then explicitly publish.
Rebuild `web/dist` after frontend changes and the server after Rust changes.
For a Vite development server, see [the web guide](../web/README.md).

## Accounts and persistent rooms

Create a dedicated local development database using credentials you control.
This setup modifies its schema; never use a production/shared database.

```sh
export DATABASE_URL='postgres://YOUR_USER:YOUR_PASSWORD@127.0.0.1:5432/simplestchat_dev?sslmode=disable'
export JWT_SECRET="$(openssl rand -hex 32)"
export RUN_MIGRATIONS=true
export REGISTRATION_ENABLED=true
ANNOUNCE_IP="$(ipconfig getifaddr en0)" MEDIA_WORKERS=1 ALLOW_AD_HOC_ROOMS=true ./target/release/simplestChat
```

The schema includes `pg_trgm`; the development role needs permission to install
that trusted extension. Reuse the same JWT secret across restarts if preserving
existing tokens matters. Do not print credentials or add them to tracked files.
Use `sslmode=disable` only for loopback development.

With no database the server is anonymous-only, and the directory explains why
persistent rooms are unavailable. Auth needs both a database and a JWT secret of
at least 32 bytes. `RUN_MIGRATIONS` and registration default false.

Passkeys additionally require `WEBAUTHN_RP_ID=localhost` and
`WEBAUTHN_ORIGIN=http://localhost:3000`, matching the actual browser origin.
Email/password login does not require passkey settings.

For a second instance without stopping an existing service, use unused ports:

```sh
PORT=3109 MEDIA_WORKERS=1 WEBRTC_SERVER_PORT_BASE=41000 ANNOUNCE_IP="$(ipconfig getifaddr en0)" ALLOW_AD_HOC_ROOMS=true ./target/release/simplestChat
```

Browse `http://localhost:3109`, adjust the passkey origin, and use a separate
database when account/room data must also be isolated. Check port availability
first; never stop somebody else's process to make a test fit.

## Passwords and recovery

In **Account**, enter the current password and select **Generate recovery key**.
Store the displayed key privately: it is shown once, not emailed, and generating
another replaces it. **Recover with a saved key** consumes that key and sets a
new password. Recovery and password changes invalidate existing account sessions.

New passwords must be 8–128 UTF-8 bytes, match confirmation and contain no control
characters. Passkey-only accounts can sign in with a passkey but cannot use
current-password-protected controls without a password. Email ownership is not
verified; there is no forgot-password email flow. Keep recovery keys out of logs,
screenshots and chat.

## Next checks

Use [testing](testing.md) for unit/database/browser/container commands,
[configuration](configuration.md) for defaults, and [deployment](deployment.md)
before exposing any service. Build the optional synthetic client separately:

```sh
cargo build --locked --release --features load-test --bin load_test
```

See [performance](performance.md) before drawing conclusions from its output.
