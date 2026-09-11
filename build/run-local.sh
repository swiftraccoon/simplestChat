#!/bin/sh
# Build and run the local UI without relying on Homebrew's Cargo/rustc selection.
set -eu

usage() {
    echo 'Usage: build/run-local.sh [--skip-web]'
    echo 'Build the web UI and start the debug server at http://localhost:3000.'
    echo '  --skip-web  Reuse an existing web/dist build; skip npm install/build.'
    echo 'Environment: ANNOUNCE_IP, PORT, MEDIA_WORKERS, WEBRTC_SERVER_PORT_BASE.'
    echo 'Database/auth settings are inherited, but DATABASE_URL must be local.'
    echo 'HTTP stays on 127.0.0.1. No database is created or migrations enabled.'
}

skip_web=0
for argument in "$@"; do
    case "$argument" in
        --skip-web) skip_web=1 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown argument: $argument" >&2; usage >&2; exit 2 ;;
    esac
done

repo_root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$repo_root"
for dependency in rustup node; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "Missing $dependency; see docs/development.md for prerequisites." >&2
        exit 2
    fi
done
if [ "$skip_web" = 0 ] && ! command -v npm >/dev/null 2>&1; then
    echo 'Missing npm; see docs/development.md for prerequisites.' >&2
    exit 2
fi
if [ "$skip_web" = 1 ] && [ ! -f web/dist/index.html ]; then
    echo '--skip-web requires web/dist/index.html; run without --skip-web first.' >&2
    exit 2
fi

toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' rust-toolchain.toml)"
if [ -z "$toolchain" ]; then
    echo 'Cannot read the Rust channel from rust-toolchain.toml.' >&2
    exit 2
fi
export OPENSSL_DIR="$repo_root/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$repo_root/build/pip-constraints.txt"
if [ ! -f "$PIP_CONSTRAINT" ]; then
    echo 'Missing build/pip-constraints.txt; restore the pinned native prerequisites.' >&2
    exit 2
fi

if [ -z "${ANNOUNCE_IP:-}" ] && [ "$(uname -s)" = Darwin ]; then
    local_interface="$(route -n get default 2>/dev/null | awk '/interface:/ { print $2; exit }' || true)"
    if [ -n "$local_interface" ]; then
        ANNOUNCE_IP="$(ipconfig getifaddr "$local_interface" 2>/dev/null || true)"
    fi
    if [ -z "${ANNOUNCE_IP:-}" ]; then
        ANNOUNCE_IP="$(ipconfig getifaddr en0 2>/dev/null || true)"
    fi
fi
if [ -z "${ANNOUNCE_IP:-}" ]; then
    echo 'Cannot detect a LAN address. Set ANNOUNCE_IP to your active interface address.' >&2
    exit 2
fi
export ANNOUNCE_IP
export BIND_ADDR=127.0.0.1
export PORT="${PORT:-3000}"
export MEDIA_WORKERS="${MEDIA_WORKERS:-1}"
export WEBRTC_SERVER_PORT_BASE="${WEBRTC_SERVER_PORT_BASE:-40000}"
export ALLOW_AD_HOC_ROOMS="${ALLOW_AD_HOC_ROOMS:-true}"

# Validate before installing/building. Probe only local sockets; never stop a
# process to free a port. The server's final bind remains authoritative.
node --input-type=module <<'JS'
import net from 'node:net';
import dgram from 'node:dgram';
const env = process.env;
const integer = (name, maximum) => {
    const value = env[name];
    if (!/^[1-9][0-9]*$/.test(value) || Number(value) > maximum) {
        throw new Error(`${name} must be an integer from 1 through ${maximum}.`);
    }
    return Number(value);
};
try {
    const port = integer('PORT', 65535);
    const workers = integer('MEDIA_WORKERS', 64);
    const media = integer('WEBRTC_SERVER_PORT_BASE', 65535);
    if (media + workers - 1 > 65535) throw new Error('The media worker port range exceeds 65535.');
    if (net.isIP(env.ANNOUNCE_IP) !== 4) throw new Error('ANNOUNCE_IP must be a literal IPv4 address for the IPv4 media sockets.');
    if (env.DATABASE_URL !== undefined) {
        if (!env.DATABASE_URL) throw new Error('DATABASE_URL is empty; unset it for guest-only mode.');
        let url;
        try { url = new URL(env.DATABASE_URL); } catch { throw new Error('Invalid local DATABASE_URL.'); }
        if (!['postgres:', 'postgresql:'].includes(url.protocol)
            || !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
            || url.hash || url.pathname.length < 2
            || [...url.searchParams.keys()].some(key => key !== 'sslmode')) {
            throw new Error('DATABASE_URL must name a local PostgreSQL database; only the sslmode query option is accepted.');
        }
    }
    const tcp = net.createServer();
    try {
        await new Promise((resolve, reject) => tcp.once('error', reject).listen(port, '127.0.0.1', resolve));
    } finally {
        if (tcp.listening) await new Promise(resolve => tcp.close(resolve));
    }
    for (let index = 0; index < workers; index++) {
        const udp = dgram.createSocket('udp4');
        try {
            await new Promise((resolve, reject) => udp.once('error', reject).bind(media + index, '0.0.0.0', resolve));
        } finally {
            try { udp.close(); } catch {}
        }
    }
} catch (error) {
    console.error(`Cannot launch locally: ${error.message}`);
    process.exitCode = 2;
}
JS

# Force both compiler paths as well as Cargo: a Homebrew rustc ahead of the
# rustup proxies must not override the repository's pinned patch release.
if ! RUSTC="$(rustup which --toolchain "$toolchain" rustc)" \
    || ! RUSTDOC="$(rustup which --toolchain "$toolchain" rustdoc)" \
    || [ ! -x "$RUSTC" ] || [ ! -x "$RUSTDOC" ]; then
    echo "Rust $toolchain is unavailable; install it with: rustup toolchain install $toolchain" >&2
    exit 2
fi
export RUSTC RUSTDOC

if [ ! -f "$OPENSSL_DIR/lib/libssl.a" ] || [ ! -f "$OPENSSL_DIR/lib/libcrypto.a" ]; then
    echo 'Installing the checksum-pinned static OpenSSL build (first launch may take several minutes).'
    "$repo_root/build/install-openssl.sh" "$OPENSSL_DIR"
fi
if [ "$skip_web" = 0 ]; then
    npm --prefix web ci --ignore-scripts
    npm --prefix web run build
fi

echo "Starting http://localhost:$PORT (HTTP loopback only; media advertised as $ANNOUNCE_IP)."
echo "Using Rust $toolchain and $MEDIA_WORKERS media worker(s). Press Ctrl-C to stop."
exec rustup run "$toolchain" cargo run --locked --bin simplestChat
