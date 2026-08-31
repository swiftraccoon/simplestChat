#!/bin/sh
set -eu

# Keep this in sync with the minimum enforced by the vendored mediasoup-sys
# build. OpenSSL 3.5 is LTS through 2030-04-08.
openssl_version='3.5.8'
openssl_sha256='a8f84a39918ec6415ce765d9b429d313ba97b8143169c172e734b9514464f5b2'
openssl_url="https://github.com/openssl/openssl/releases/download/openssl-${openssl_version}/openssl-${openssl_version}.tar.gz"

install_prefix=${1:-}
if [ -z "$install_prefix" ]; then
    echo "usage: $0 ABSOLUTE_INSTALL_PREFIX" >&2
    exit 2
fi

case "$install_prefix" in
    /*) ;;
    *)
        echo "OpenSSL install prefix must be absolute: $install_prefix" >&2
        exit 2
        ;;
esac

case "$install_prefix" in
    /|/usr|/usr/local|/opt)
        echo "refusing broad OpenSSL install prefix: $install_prefix" >&2
        exit 2
        ;;
esac

build_dir=$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-openssl.XXXXXXXX")
cleanup() {
    rm -rf "$build_dir"
}
trap cleanup EXIT HUP INT TERM

archive="$build_dir/openssl-${openssl_version}.tar.gz"
curl --fail --show-error --location --proto '=https' --tlsv1.2 \
    "$openssl_url" --output "$archive"

if command -v sha256sum >/dev/null 2>&1; then
    actual_sha256=$(sha256sum "$archive" | awk '{print $1}')
else
    actual_sha256=$(shasum -a 256 "$archive" | awk '{print $1}')
fi

if [ "$actual_sha256" != "$openssl_sha256" ]; then
    echo "OpenSSL archive SHA-256 mismatch" >&2
    echo "expected: $openssl_sha256" >&2
    echo "actual:   $actual_sha256" >&2
    exit 1
fi

tar -xzf "$archive" -C "$build_dir"
cd "$build_dir/openssl-${openssl_version}"

# A normalized lib directory makes OPENSSL_DIR and PKG_CONFIG_PATH identical
# on Fedora, Ubuntu, x86_64, and aarch64. `no-shared` is essential: the final
# binary must not silently load an older runtime libssl with the same SONAME.
./Configure \
    --prefix="$install_prefix" \
    --openssldir="$install_prefix/ssl" \
    --libdir=lib \
    no-shared \
    no-tests

if command -v nproc >/dev/null 2>&1; then
    build_jobs=$(nproc)
elif command -v sysctl >/dev/null 2>&1; then
    build_jobs=$(sysctl -n hw.ncpu)
else
    build_jobs=1
fi

make -s -j "$build_jobs"
make -s install_sw

test -f "$install_prefix/lib/libssl.a"
test -f "$install_prefix/lib/libcrypto.a"
PKG_CONFIG_PATH="$install_prefix/lib/pkgconfig" \
    pkg-config --atleast-version="$openssl_version" openssl
"$install_prefix/bin/openssl" version
