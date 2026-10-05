#!/bin/sh
set -eu

awslc_version='5.11.0'
awslc_sha256='8cb24c6e6be1fa7ff05075c4560ca8b537a7ef48f9e6f465af4ea455794d74f4'
awslc_prefix='simplestchat_awslc_5_11_0'
bindgen_version='0.73.2'
bindgen_sha256='5622e043625b6d1f32c84f53699690a44d1419e657879de26eb252699f5ebb7c'
install_prefix=${1:-}
case "$install_prefix" in
    /|/usr|/usr/local|/opt|'') echo "usage: $0 ABSOLUTE_PRIVATE_INSTALL_PREFIX" >&2; exit 2 ;;
    /*) ;;
    *) echo 'AWS-LC install prefix must be absolute' >&2; exit 2 ;;
esac
if [ -e "$install_prefix" ] || [ -L "$install_prefix" ]; then
    echo 'AWS-LC destination already exists; retain it or select a new prefix.' >&2
    exit 2
fi
install_parent=$(dirname "$install_prefix")
mkdir -p "$install_parent"
python3 - "$install_parent" <<'PY'
import os, pathlib, stat, sys
path = pathlib.Path(sys.argv[1])
info = path.lstat()
if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
    raise SystemExit("AWS-LC installation parent must be an owned directory, not writable by others")
PY
build_dir=$(mktemp -d "$install_parent/.aws-lc-build.XXXXXXXX")
cleanup() { rm -rf "$build_dir"; }
trap cleanup EXIT HUP INT TERM
build_jobs=${AWS_LC_BUILD_JOBS:-4}
case "$build_jobs" in ''|*[!0-9]*) echo 'AWS_LC_BUILD_JOBS must be 1 through 64' >&2; exit 2 ;; esac
if [ "$build_jobs" -lt 1 ] || [ "$build_jobs" -gt 64 ]; then exit 2; fi

download() {
    curl --fail --show-error --location --proto '=https' --tlsv1.2 \
        --retry 3 --retry-max-time 180 --connect-timeout 20 --max-time 120 \
        "$1" --output "$2"
    python3 - "$2" "$3" <<'PY'
import hashlib, pathlib, sys
archive = pathlib.Path(sys.argv[1])
if archive.stat().st_size > 192 * 1024 * 1024:
    raise SystemExit("AWS-LC build input exceeds its size limit")
if hashlib.sha256(archive.read_bytes()).hexdigest() != sys.argv[2]:
    raise SystemExit("AWS-LC build input SHA-256 mismatch")
PY
}
archive="$build_dir/aws-lc.tar.gz"
binding_archive="$build_dir/bindgen-cli.crate"
download "https://github.com/aws/aws-lc/archive/refs/tags/v${awslc_version}.tar.gz" "$archive" "$awslc_sha256"
download "https://static.crates.io/crates/bindgen-cli/bindgen-cli-${bindgen_version}.crate" "$binding_archive" "$bindgen_sha256"
python3 -m tarfile --filter data --extract "$archive" "$build_dir"
python3 -m tarfile --filter data --extract "$binding_archive" "$build_dir"

# Cargo verifies every locked generator dependency. The CLI source itself is
# authenticated above; neither a global install nor an ambient bindgen is used.
CARGO_PROFILE_RELEASE_STRIP=none cargo install --locked \
    --path "$build_dir/bindgen-cli-${bindgen_version}" \
    --root "$build_dir/bindgen" --target-dir "$build_dir/bindgen-build" --jobs "$build_jobs"
PATH="$build_dir/bindgen/bin:$PATH"
export PATH
test "$(bindgen --version)" = "bindgen ${bindgen_version}"
source_dir="$build_dir/aws-lc-${awslc_version}"
cmake -S "$source_dir" -B "$build_dir/unprefixed" \
    -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF \
    -DBUILD_TOOL=OFF -DBUILD_LIBSSL=OFF -DDISABLE_GO=ON -DENABLE_SOURCE_MODIFICATION=OFF
cmake --build "$build_dir/unprefixed" --target crypto --parallel "$build_jobs"
(
    cd "$source_dir"
    GOTOOLCHAIN=local GOFLAGS=-mod=readonly go run ./util/read_symbols.go \
        -out "$build_dir/symbols.txt" "$build_dir/unprefixed/crypto/libcrypto.a"
)

staging="$build_dir/install"
cmake -S "$source_dir" -B "$build_dir/prefixed" \
    -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF \
    -DBUILD_TOOL=OFF -DBUILD_LIBSSL=OFF -DENABLE_SOURCE_MODIFICATION=OFF \
    -DBUILD_AWSLC_PROVIDER=OFF -DENABLE_CRYPTO_POLICIES=OFF \
    -DGENERATE_RUST_BINDINGS=ON -DRUST_BINDINGS_TARGET_VERSION=1.70 \
    -DBORINGSSL_PREFIX="$awslc_prefix" -DBORINGSSL_PREFIX_SYMBOLS="$build_dir/symbols.txt" \
    -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_INSTALL_PREFIX="$staging"
cmake --build "$build_dir/prefixed" --target crypto rust_bindings --parallel "$build_jobs"
cmake --install "$build_dir/prefixed"

# The wrapper explicitly supports this archive basename. Keeping it distinct
# also makes linker search paths unambiguous beside the application's OpenSSL.
mv "$staging/lib/libcrypto.a" "$staging/lib/libcrypto-awslc.a"
rm -rf "$staging/lib/pkgconfig" "$staging/lib/cmake"
test -f "$staging/share/rust/aws_lc_bindings.rs"
evidence="$staging/share/simplestchat"
mkdir -p "$evidence"
cp "$archive" "$evidence/aws-lc-source.tar.gz"
cp "$binding_archive" "$evidence/bindgen-cli.crate"
cp "$build_dir/prefixed/CMakeCache.txt" "$evidence/CMakeCache.txt"
cp "$build_dir/symbols.txt" "$evidence/symbols.txt"
python3 - "$staging" "$0" "$build_dir/bindgen/bin/bindgen" <<'PY'
import hashlib, json, pathlib, sys
prefix, installer, bindgen = map(pathlib.Path, sys.argv[1:])
cache = prefix / "share/simplestchat/CMakeCache.txt"
options = {}
for line in cache.read_text().splitlines():
    if line and not line.startswith(("#", "//")) and "=" in line and ":" in line:
        key, value = line.split("=", 1)
        options[key.split(":", 1)[0]] = value
keys = ["CMAKE_BUILD_TYPE", "BUILD_SHARED_LIBS", "BUILD_TESTING", "BUILD_TOOL",
        "BUILD_LIBSSL", "ENABLE_SOURCE_MODIFICATION", "BUILD_AWSLC_PROVIDER",
        "ENABLE_CRYPTO_POLICIES", "GENERATE_RUST_BINDINGS", "RUST_BINDINGS_TARGET_VERSION",
        "BORINGSSL_PREFIX", "CMAKE_POSITION_INDEPENDENT_CODE", "CMAKE_INSTALL_LIBDIR"]
files = ["lib/libcrypto-awslc.a", "share/rust/aws_lc_bindings.rs",
         "include/openssl/base.h", "include/openssl/boringssl_prefix_symbols.h",
         "share/simplestchat/aws-lc-source.tar.gz", "share/simplestchat/bindgen-cli.crate",
         "share/simplestchat/CMakeCache.txt", "share/simplestchat/symbols.txt"]
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
receipt = {"configure_options": {key: options[key] for key in keys},
           "installer_sha256": digest(installer), "bindgen_sha256": digest(bindgen),
           "files": {name: digest(prefix / name) for name in files}}
(prefix / "share/simplestchat/build.json").write_text(json.dumps(receipt, indent=2) + "\n")
PY
if [ -e "$install_prefix" ] || [ -L "$install_prefix" ]; then
    echo 'AWS-LC destination appeared during the build; refusing replacement.' >&2
    exit 2
fi
mv "$staging" "$install_prefix"
echo "Installed authenticated AWS-LC ${awslc_version} at $install_prefix"
