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

# The SHA-256 of standard input as 64 lowercase hexadecimal digits; fails
# when no digest tool answers in that form.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256_answer=$(sha256sum) || return 1
    else
        sha256_answer=$(shasum -a 256) || return 1
    fi
    sha256_answer=${sha256_answer%% *}
    case $sha256_answer in
        *[!0123456789abcdef]*) return 1 ;;
    esac
    [ "${#sha256_answer}" -eq 64 ] || return 1
    printf '%s\n' "$sha256_answer"
}

# Whether $1 is a regular file and not a symbolic link.
regular() {
    [ -f "$1" ] && [ ! -L "$1" ]
}

# The value that the CMake cache $1 records last for the option $2: the text
# after the first `=` of a line that is not empty or a `#` or `//` comment,
# holds a `:` and names the option before the first `:` of its key. Fails
# when no line names it or the value holds anything but letters, digits,
# `.`, `_` and `-`, the characters a receipt records unescaped.
cache_option() {
    cache_found=''
    cache_value=''
    while IFS= read -r cache_line || [ -n "$cache_line" ]; do
        case $cache_line in
            '' | '#'* | '//'*) continue ;;
            *=*) ;;
            *) continue ;;
        esac
        case $cache_line in
            *:*) ;;
            *) continue ;;
        esac
        cache_key=${cache_line%%=*}
        if [ "${cache_key%%:*}" = "$2" ]; then
            cache_value=${cache_line#*=}
            cache_found=1
        fi
    done < "$1"
    [ -n "$cache_found" ] || return 1
    case $cache_value in
        *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-]*) return 1 ;;
    esac
    printf '%s\n' "$cache_value"
}

# Print the receipt of the installation at $1 as JSON with two-space
# indentation: the reviewed options as its own CMake cache records them, the
# SHA-256 of the installer $2, the binding generator's SHA-256 $3 and the
# SHA-256 of every recorded file, each a regular file. Fails, possibly after
# printing part of it, when any of them is missing or malformed.
receipt_text() {
    receipt_cache="$1/share/simplestchat/CMakeCache.txt"
    regular "$receipt_cache" || return 1
    receipt_installer=$(sha256_of < "$2") || return 1
    case $3 in
        *[!0123456789abcdef]*) return 1 ;;
    esac
    [ "${#3}" -eq 64 ] || return 1
    printf '{\n  "configure_options": {'
    receipt_separator=''
    for receipt_option in CMAKE_BUILD_TYPE BUILD_SHARED_LIBS BUILD_TESTING BUILD_TOOL \
        BUILD_LIBSSL ENABLE_SOURCE_MODIFICATION BUILD_AWSLC_PROVIDER \
        ENABLE_CRYPTO_POLICIES GENERATE_RUST_BINDINGS RUST_BINDINGS_TARGET_VERSION \
        BORINGSSL_PREFIX CMAKE_POSITION_INDEPENDENT_CODE CMAKE_INSTALL_LIBDIR; do
        receipt_value=$(cache_option "$receipt_cache" "$receipt_option") || return 1
        printf '%s\n    "%s": "%s"' "$receipt_separator" "$receipt_option" "$receipt_value"
        receipt_separator=','
    done
    printf '\n  },\n  "installer_sha256": "%s",\n' "$receipt_installer"
    printf '  "bindgen_sha256": "%s",\n  "files": {' "$3"
    receipt_separator=''
    for receipt_file in lib/libcrypto-awslc.a share/rust/aws_lc_bindings.rs \
        include/openssl/base.h include/openssl/boringssl_prefix_symbols.h \
        share/simplestchat/aws-lc-source.tar.gz share/simplestchat/bindgen-cli.crate \
        share/simplestchat/CMakeCache.txt share/simplestchat/symbols.txt; do
        regular "$1/$receipt_file" || return 1
        receipt_digest=$(sha256_of < "$1/$receipt_file") || return 1
        printf '%s\n    "%s": "%s"' "$receipt_separator" "$receipt_file" "$receipt_digest"
        receipt_separator=','
    done
    printf '\n  }\n}\n'
}

# Whether the receipt $2 of the installation at $1 is exactly what this
# installer writes for that installation now, with the binding generator
# digest that the receipt names: the same installer, every recorded file
# intact and regular, and nothing else.
receipt_complete() {
    regular "$2" || return 1
    complete_generator=''
    while IFS= read -r complete_line || [ -n "$complete_line" ]; do
        case $complete_line in
            '  "bindgen_sha256": "'*'",')
                complete_generator=${complete_line#'  "bindgen_sha256": "'}
                complete_generator=${complete_generator%'",'}
                break
                ;;
        esac
    done < "$2"
    complete_expected=$(receipt_text "$1" "$0" "$complete_generator") || return 1
    complete_recorded=$(sha256_of < "$2") || return 1
    [ "$(printf '%s\n' "$complete_expected" | sha256_of)" = "$complete_recorded" ]
}

# Whether $1 is a directory, not a symbolic link, owned by the real user and
# writable by neither its group nor others.
owned_directory() {
    owned_listing=$(ls -ldn "$1") || return 1
    set -f
    # shellcheck disable=SC2086 # The listing's fields are split on purpose.
    set -- $owned_listing
    set +f
    case $1 in
        d????w* | d???????w*) return 1 ;;
        d*) ;;
        *) return 1 ;;
    esac
    [ "$#" -ge 3 ] && [ "$3" = "$(id -ru)" ]
}

# A prefix this installer produced carries share/simplestchat/build.json. The
# same installer with every recorded file intact is retained (a cache restored
# it beside a missed dedicated cache); another installer's or a damaged prefix
# is rebuilt and replaced only after the new build succeeds; anything else
# (an unknown directory, a symbolic link) is never touched.
replace_existing=0
if [ -L "$install_prefix" ]; then
    echo 'AWS-LC destination is a symbolic link; refusing to replace it.' >&2
    exit 2
fi
if [ -e "$install_prefix" ]; then
    receipt="$install_prefix/share/simplestchat/build.json"
    if [ ! -f "$receipt" ]; then
        echo 'AWS-LC destination already exists; retain it or select a new prefix.' >&2
        exit 2
    fi
    if receipt_complete "$install_prefix" "$receipt"; then
        echo "AWS-LC ${awslc_version} is already installed by this installer at $install_prefix; retaining it." >&2
        exit 0
    fi
    echo "Replacing the AWS-LC installation at $install_prefix after this build succeeds: its receipt names another installer or is incomplete." >&2
    replace_existing=1
fi
install_parent=$(dirname "$install_prefix")
mkdir -p "$install_parent"
if ! owned_directory "$install_parent"; then
    echo 'AWS-LC installation parent must be an owned directory, not writable by others' >&2
    exit 1
fi
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
    # A size that is not a decimal count of at most 192 MiB is too large.
    download_size=$(wc -c < "$2")
    download_size=${download_size##* }
    if ! [ "$download_size" -le 201326592 ] 2>/dev/null; then
        echo 'AWS-LC build input exceeds its size limit' >&2
        exit 1
    fi
    if ! download_digest=$(sha256_of < "$2") || [ "$download_digest" != "$3" ]; then
        echo 'AWS-LC build input SHA-256 mismatch' >&2
        exit 1
    fi
}

# Unpack the authenticated archive $1 into $2. Every member must be a file
# its owner can read and write, executable by others only if by its owner,
# or a directory everyone can read and search; links, devices, FIFOs and
# set-id or sticky modes are refused, as are absolute names and `..`
# components. No owner is restored, and modes are limited by umask 022.
unpack() {
    LC_ALL=C tar -tvzf "$1" > "$build_dir/archive-members"
    while IFS= read -r unpack_member || [ -n "$unpack_member" ]; do
        case $unpack_member in
            -rwx??[-x]??[-x]* | -rw-??-??-* | drwxr?xr?x*) ;;
            *) echo 'AWS-LC build input holds a member other than a plain file or directory' >&2; exit 1 ;;
        esac
    done < "$build_dir/archive-members"
    LC_ALL=C tar -tPzf "$1" > "$build_dir/archive-names"
    while IFS= read -r unpack_name || [ -n "$unpack_name" ]; do
        case /$unpack_name/ in
            //* | */../*) echo 'AWS-LC build input names a path outside its directory' >&2; exit 1 ;;
        esac
    done < "$build_dir/archive-names"
    (umask 022 && tar --no-same-owner --no-same-permissions -xzf "$1" -C "$2")
}
archive="$build_dir/aws-lc.tar.gz"
binding_archive="$build_dir/bindgen-cli.crate"
download "https://github.com/aws/aws-lc/archive/refs/tags/v${awslc_version}.tar.gz" "$archive" "$awslc_sha256"
download "https://static.crates.io/crates/bindgen-cli/bindgen-cli-${bindgen_version}.crate" "$binding_archive" "$bindgen_sha256"
unpack "$archive" "$build_dir"
unpack "$binding_archive" "$build_dir"

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
# The receipt records the cache's own values, so the cache must be plain
# text: tabs, line feeds and printable ASCII only.
LC_ALL=C tr -d '\011\012\040-\176' < "$evidence/CMakeCache.txt" > "$build_dir/cache-other-bytes"
if [ -s "$build_dir/cache-other-bytes" ]; then
    echo 'AWS-LC build cache is not plain ASCII text' >&2
    exit 1
fi
bindgen_digest=$(sha256_of < "$build_dir/bindgen/bin/bindgen")
receipt_text "$staging" "$0" "$bindgen_digest" > "$evidence/build.json"
if [ "$replace_existing" = 1 ]; then
    rm -rf "$install_prefix"
fi
if [ -e "$install_prefix" ] || [ -L "$install_prefix" ]; then
    echo 'AWS-LC destination appeared during the build; refusing replacement.' >&2
    exit 2
fi
mv "$staging" "$install_prefix"
echo "Installed authenticated AWS-LC ${awslc_version} at $install_prefix"
