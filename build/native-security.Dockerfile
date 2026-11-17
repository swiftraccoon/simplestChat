# Source archives and the base manifest are pinned in security/native/toolchain.json.
# This image is a disposable local test tool; it never becomes the release image.
FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS native-base
RUN dnf install -y --setopt=install_weak_deps=False \
    ca-certificates curl gcc gcc-c++ glibc-devel libstdc++-devel libstdc++-static \
    make perl pkgconf-pkg-config python3 python3-pip tar xz zstd libxml2 zlib libzstd \
    && dnf clean all
FROM native-base AS llvm-toolchain
# A bounded, checksum-authenticated upstream LLVM archive supplies clang,
# compiler-rt, libFuzzer and a matching symbolizer. No PATH compiler fallback.
# The host passes the archive that toolchain.json pins for its architecture.
ARG LLVM_URL
ARG LLVM_SHA256
ARG LLVM_BYTES
ARG LLVM_DIRECTORY
RUN /bin/sh -eu <<'SH'
: "${LLVM_URL:?}" "${LLVM_SHA256:?}" "${LLVM_BYTES:?}" "${LLVM_DIRECTORY:?}"
set -C
status=0
curl --fail --location --proto '=https' --proto-redir '=https' --silent --show-error \
  --connect-timeout 60 --speed-limit 1 --speed-time 60 --max-filesize "$LLVM_BYTES" \
  -- "$LLVM_URL" > /tmp/llvm.tar.zst || status=$?
# curl refuses a declared size over the limit with 63; one that only shows
# while it arrives ends the transfer at the limit, with 63 or 56 by version.
if [ "$status" -ne 0 ]; then
  if [ "$status" -eq 63 ] || [ "$(stat --format=%s /tmp/llvm.tar.zst)" -ge "$LLVM_BYTES" ]; then
    echo 'LLVM archive exceeds pinned size' >&2
  fi
  exit 1
fi
[ "$(stat --format=%s /tmp/llvm.tar.zst)" = "$LLVM_BYTES" ] || {
  echo 'LLVM archive does not match pinned bytes' >&2
  exit 1
}
case "$LLVM_SHA256" in
  *[!0-9a-f]*) echo 'LLVM archive does not match pinned bytes' >&2; exit 1 ;;
esac
printf '%s  %s\n' "$LLVM_SHA256" /tmp/llvm.tar.zst | sha256sum --check --strict --status || {
  echo 'LLVM archive does not match pinned bytes' >&2
  exit 1
}
SH
RUN /bin/sh -eu <<'SH'
: "${LLVM_DIRECTORY:?}"
mkdir /opt/llvm
timeout 180 tar --use-compress-program='zstd --long=30' -tf /tmp/llvm.tar.zst \
  > /tmp/llvm-listing.txt
if [ "$(stat --format=%s /tmp/llvm-listing.txt)" -gt 8388608 ]; then
  echo 'LLVM archive member listing exceeds budget' >&2
  exit 1
fi
prefix="$LLVM_DIRECTORY/"
while IFS= read -r name || [ -n "$name" ]; do
  case "$name" in
    "$prefix"*) ;;
    *) echo 'Unexpected LLVM archive member layout' >&2; exit 1 ;;
  esac
  case "/$name/" in
    */../*) echo 'Unexpected LLVM archive member layout' >&2; exit 1 ;;
  esac
  case "${name#"$prefix"}" in
    lib/clang/* | LICENSE.TXT | lib/libLLVM*.so* | lib/libclang-cpp*.so*) printf '%s\n' "$name" ;;
    bin/clang | bin/clang++ | bin/clang-23 | bin/llvm-symbolizer | bin/llvm-ar | bin/llvm-ranlib)
      printf '%s\n' "$name" ;;
  esac
done < /tmp/llvm-listing.txt > /tmp/llvm-members.txt
timeout 300 tar --use-compress-program='zstd --long=30' -xf /tmp/llvm.tar.zst -C /opt/llvm \
  --strip-components=1 --no-recursion --files-from=/tmp/llvm-members.txt
rm /tmp/llvm.tar.zst /tmp/llvm-listing.txt
for tool in clang clang++ llvm-symbolizer; do
  printf '%s %s\n' "$tool" "$(realpath -m "/opt/llvm/bin/$tool")"
  timeout 10 "/opt/llvm/bin/$tool" --version
done
SH
FROM native-base
COPY --from=llvm-toolchain /opt/llvm /opt/llvm
COPY build/install-openssl.sh /opt/check/build/install-openssl.sh
RUN /opt/check/build/install-openssl.sh /opt/openssl-4.1.0
ENV PATH=/opt/llvm/bin:/usr/bin:/bin \
    OPENSSL_DIR=/opt/openssl-4.1.0 OPENSSL_STATIC=1 \
    PKG_CONFIG_PATH=/opt/openssl-4.1.0/lib/pkgconfig \
    CC=/opt/llvm/bin/clang CXX=/opt/llvm/bin/clang++ \
    PYTHON=/usr/bin/python3 PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1 \
    PIP_CONFIG_FILE=/dev/null PIP_INDEX_URL=https://pypi.org/simple \
    PIP_CONSTRAINT=/opt/check/build/pip-constraints.txt \
    MEDIASOUP_OUT_DIR=/opt/native-tools \
    MEDIASOUP_INSTALL_DIR=/work/install BUILD_DIR=/work/build \
    PYTHONPATH=/opt/native-tools/pip_invoke:/opt/native-tools/pip_meson_ninja \
    ASAN_SYMBOLIZER_PATH=/opt/llvm/bin/llvm-symbolizer \
    UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1
COPY build/pip-constraints.txt /opt/check/build/pip-constraints.txt
COPY vendor/mediasoup-sys-0.19.0 /opt/worker
RUN python3 -m pip install --no-user --target /opt/native-tools/pip_invoke \
      --require-hashes --only-binary=:all: \
      --requirement /opt/worker/python-invoke-requirements.txt \
    && python3 -m invoke --search-root /opt/worker meson-ninja \
    && cd /opt/worker \
    && /opt/native-tools/pip_meson_ninja/bin/meson subprojects download
# System library identities complement the fixed LLVM and source pins; this
# receipt describes the resolved builder, not a promise that RPM repositories freeze.
RUN rpm -qa --qf '%{NAME} %{EPOCHNUM}:%{VERSION}-%{RELEASE} %{ARCH}\n' \
    | sort > /opt/check/builder-rpms.txt
RUN chmod -R a+rX /opt/check /opt/worker
# Nothing of the repository's own runs here: the host issues every step in
# the sandbox, by these absolute paths and with the GNU options it gives them,
# and the image is refused unless each of them works. The image has no user
# of its own, so this runs as the user the recipe builds with.
RUN /bin/sh -eu <<'SH'
for program in /usr/bin/sleep /bin/sh /usr/bin/cat /usr/bin/env /usr/bin/stat /usr/bin/head \
    /usr/bin/sha256sum /usr/bin/cp /usr/bin/mkdir /usr/bin/chmod; do
  [ -x "$program" ] || {
    echo "Missing sandbox program $program" >&2
    exit 1
  }
done
rpms=/opt/check/builder-rpms.txt
case "$(/usr/bin/stat '--format=%f %s %.9Y' "$rpms")" in
  ?*' '[0-9]*' '[0-9]*.[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]) ;;
  *) echo 'Sandbox stat lacks the host format' >&2; exit 1 ;;
esac
[ "$(/usr/bin/stat --format=%s "$rpms")" -gt 0 ]
[ "$(/usr/bin/head --bytes=1 "$rpms" | wc -c)" -eq 1 ]
/usr/bin/sha256sum "$rpms" > /dev/null
/usr/bin/env -0 > /dev/null
/usr/bin/env -i CHECKED=1 /usr/bin/sleep 0
work=/tmp/sandbox-programs
/usr/bin/mkdir "$work"
/usr/bin/mkdir -p "$work/tree/nested"
echo written | /bin/sh -c 'umask 077 && set -C && exec /usr/bin/cat > "$1"' sh \
  "$work/tree/nested/file"
if echo again | /bin/sh -c 'umask 077 && set -C && exec /usr/bin/cat > "$1"' sh \
    "$work/tree/nested/file" 2> /dev/null; then
  echo 'Sandbox shell writes over a file' >&2
  exit 1
fi
/usr/bin/chmod 700 "$work/tree/nested/file"
/usr/bin/cp -R --preserve=mode,timestamps "$work/tree" "$work/copy"
[ "$(/usr/bin/stat '--format=%f %s %.9Y' "$work/tree/nested/file")" = \
  "$(/usr/bin/stat '--format=%f %s %.9Y' "$work/copy/nested/file")" ]
rm -r "$work"
SH
WORKDIR /work
ENTRYPOINT ["/usr/bin/sleep"]
CMD ["2100"]
