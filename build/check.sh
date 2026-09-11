#!/bin/sh
# Shared local/CI quality gates. Does not install tools or start application services.
set -eu

usage() {
    echo 'Usage: build/check.sh [--web | --rust | --helpers]'
    echo 'Default: run all three groups. Requires the development prerequisites.'
    echo 'Web: typed lint, formatting, unit tests, type checking and production build.'
    echo 'Rust: formatting, all-target/all-feature Clippy, and checked documentation.'
    echo 'Helpers: shell syntax and helper/benchmark regression tests.'
    echo 'Database, native Rust tests and browser integration remain separate; see docs/testing.md.'
}

check_group=all
if [ "$#" -gt 1 ]; then usage >&2; exit 2; fi
case "${1:-}" in
    '') ;;
    --web) check_group=web ;;
    --rust) check_group=rust ;;
    --helpers) check_group=helpers ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac

repo_root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$repo_root"

if [ "$check_group" = all ] || [ "$check_group" = web ]; then
    if [ ! -d web/node_modules ]; then
        echo 'Install web dependencies first: npm --prefix web ci --ignore-scripts' >&2
        exit 2
    fi
    npm --prefix web run lint
    npm --prefix web run format:check
    npm --prefix web test
    npm --prefix web run build
fi

if [ "$check_group" = all ] || [ "$check_group" = rust ]; then
    toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' rust-toolchain.toml)"
    if [ -z "$toolchain" ]; then
        echo 'Cannot read the pinned Rust toolchain.' >&2
        exit 2
    fi
    RUSTC="$(rustup which --toolchain "$toolchain" rustc)"
    RUSTDOC="$(rustup which --toolchain "$toolchain" rustdoc)"
    export RUSTC RUSTDOC
    # Build scripts also invoke rustfmt/rustc directly. Prevent an incompatible
    # Homebrew toolchain earlier on PATH from taking over those child commands.
    PATH="$(dirname -- "$RUSTC"):$PATH"
    export PATH
    OPENSSL_DIR="${OPENSSL_DIR:-$repo_root/target/openssl-3.5.8}"
    PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
    OPENSSL_STATIC=1
    PIP_CONSTRAINT="$repo_root/build/pip-constraints.txt"
    export OPENSSL_DIR PKG_CONFIG_PATH OPENSSL_STATIC PIP_CONSTRAINT
    if [ ! -f "$OPENSSL_DIR/lib/libssl.a" ] || [ ! -f "$OPENSSL_DIR/lib/libcrypto.a" ]; then
        echo 'Static OpenSSL is missing; follow docs/development.md#native-and-web-build.' >&2
        exit 2
    fi
    rustup run "$toolchain" cargo fmt --all -- --check
    rustup run "$toolchain" cargo clippy --locked --all-targets --all-features --no-deps -- -D warnings
    RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings"
    export RUSTDOCFLAGS
    rustup run "$toolchain" cargo doc --locked --all-features --no-deps --document-private-items
fi

if [ "$check_group" = all ] || [ "$check_group" = helpers ]; then
    for helper_script in build/check.sh build/run-local.sh; do sh -n "$helper_script"; done
    for helper_script in build/*.sh; do bash -n "$helper_script"; done
    shellcheck build/*.sh
    node --test build/tests/*.test.mjs load_tests/benchmark-local.test.mjs
fi
