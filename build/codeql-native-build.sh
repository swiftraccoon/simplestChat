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
# The driver hands over its budget's compilers, already held to this host's
# processors and memory; the traced build takes exactly those.
case "${CODEQL_BUILD_JOBS-}" in
  1 | 2 | 3 | 4) MEDIASOUP_BUILD_JOBS="$CODEQL_BUILD_JOBS" ;;
  *)
    echo 'CODEQL_BUILD_JOBS must be the driver budget: 1 to 4 compilers.' >&2
    exit 1
    ;;
esac
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
  --requirement vendor/mediasoup-sys-0.19.0/python-invoke-requirements.txt
python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 setup
# The upstream flatc prerequisite omits a job limit. Complete that same target
# within our budget first; its later invocation has no remaining compilation.
generator_started=$SECONDS
PYTHONPATH="$MEDIASOUP_OUT_DIR/pip_meson_ninja:$PYTHONPATH" \
  NINJA="$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/ninja" \
  "$MEDIASOUP_OUT_DIR/pip_meson_ninja/bin/meson" compile -C "$BUILD_DIR" \
  -j "$MEDIASOUP_BUILD_JOBS" flatbuffers-generator
printf 'CodeQL FlatBuffers generator: %ss\n' "$((SECONDS - generator_started))"
worker_started=$SECONDS
python3 -m invoke --search-root vendor/mediasoup-sys-0.19.0 libmediasoup-worker
printf 'CodeQL worker library: %ss\n' "$((SECONDS - worker_started))"
test -s "$MEDIASOUP_INSTALL_DIR/libmediasoup-worker.a"
