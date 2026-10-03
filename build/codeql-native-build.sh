#!/usr/bin/env bash
# Compile a fresh real worker while the caller's CodeQL tracer is active.
set -euo pipefail
umask 077
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"
test "$(uname -s)" = Linux
: "${OPENSSL_DIR:?A checksum-pinned static OpenSSL prefix is required}"
test -s "$OPENSSL_DIR/lib/libssl.a"
test -s "$OPENSSL_DIR/lib/libcrypto.a"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig" OPENSSL_STATIC=1
export CC=/usr/bin/gcc CXX=/usr/bin/g++
export PIP_CONFIG_FILE=/dev/null PIP_INDEX_URL=https://pypi.org/simple
export PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1
export PIP_CONSTRAINT="$project_root/build/pip-constraints.txt"
export MEDIASOUP_BUILDTYPE=Release
MEDIASOUP_BUILD_JOBS="$(python3 build/security_codeql_resources.py)"
export MEDIASOUP_BUILD_JOBS
printf 'CodeQL traced compile workers: %s\n' "$MEDIASOUP_BUILD_JOBS"
unset DOCS_RS MESON MESON_ARGS MESON_VERSION NINJA_VERSION PYTHONPATH
mkdir -p "$project_root/target"
worker_root="$(mktemp -d "$project_root/target/codeql-worker.XXXXXXXX")"
export MEDIASOUP_OUT_DIR="$worker_root/out"
export MEDIASOUP_INSTALL_DIR="$worker_root/install"
export BUILD_DIR="$worker_root/build"
export PYTHONPATH="$MEDIASOUP_OUT_DIR/pip_invoke"
python3 -m pip install --no-user --target "$PYTHONPATH" \
  --require-hashes --only-binary=:all: \
  --requirement vendor/mediasoup-sys-0.17.0/python-invoke-requirements.txt
python3 -m invoke --search-root vendor/mediasoup-sys-0.17.0 libmediasoup-worker
test -s "$MEDIASOUP_INSTALL_DIR/libmediasoup-worker.a"
