# Source archives and the base manifest are pinned in security/native/toolchain.json.
# This image is a disposable local test tool; it never becomes the release image.
FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS native-base
RUN dnf install -y --setopt=install_weak_deps=False \
    ca-certificates curl gcc gcc-c++ glibc-devel libstdc++-devel libstdc++-static \
    make perl pkgconf-pkg-config python3 python3-pip tar xz zstd libxml2 zlib libzstd \
    && dnf clean all
FROM native-base AS llvm-toolchain
COPY security/native/toolchain.json /opt/check/security/native/toolchain.json
# A bounded, checksum-authenticated upstream LLVM archive supplies clang,
# compiler-rt, libFuzzer and a matching symbolizer. No PATH compiler fallback.
RUN python3 - <<'PY'
import hashlib
import json
import platform
import fnmatch
import subprocess
import urllib.request
from pathlib import Path
pin = json.loads(Path('/opt/check/security/native/toolchain.json').read_text())
asset = pin['platforms']['linux-' + platform.machine()]
archive = Path('/tmp/llvm.tar.zst')
digest = hashlib.sha256()
size = 0
with urllib.request.urlopen(asset['url'], timeout=60) as source, archive.open('xb') as out:
    while chunk := source.read(1024 * 1024):
        size += len(chunk)
        if size > asset['bytes']:
            raise SystemExit('LLVM archive exceeds pinned size')
        digest.update(chunk)
        out.write(chunk)
if size != asset['bytes'] or digest.hexdigest() != asset['sha256']:
    raise SystemExit('LLVM archive does not match pinned bytes')
PY
RUN python3 - <<'PY'
import fnmatch
import json
import platform
import subprocess
from pathlib import Path
pin = json.loads(Path('/opt/check/security/native/toolchain.json').read_text())
asset = pin['platforms']['linux-' + platform.machine()]
archive = Path('/tmp/llvm.tar.zst')
Path('/opt/llvm').mkdir()
tar = ['tar', '--use-compress-program=zstd --long=30']
listing = subprocess.run([*tar, '-tf', str(archive)], check=True, capture_output=True, timeout=180).stdout
if len(listing) > 8 * 1024 * 1024:
    raise SystemExit('LLVM archive member listing exceeds budget')
prefix = asset['directory'] + '/'
members = []
for name in listing.decode('utf-8').splitlines():
    if not name.startswith(prefix) or '..' in Path(name).parts:
        raise SystemExit('Unexpected LLVM archive member layout')
    relative = name.removeprefix(prefix)
    if (relative.startswith('lib/clang/') or
        relative in ('bin/clang', 'bin/clang++', 'bin/clang-23', 'bin/llvm-symbolizer',
                     'bin/llvm-ar', 'bin/llvm-ranlib', 'LICENSE.TXT') or
        fnmatch.fnmatchcase(relative, 'lib/libLLVM*.so*') or
        fnmatch.fnmatchcase(relative, 'lib/libclang-cpp*.so*')):
        members.append(name)
Path('/tmp/llvm-members.txt').write_text('\n'.join(members) + '\n')
subprocess.run([*tar, '-xf', str(archive), '-C', '/opt/llvm', '--strip-components=1', '--no-recursion', '--files-from=/tmp/llvm-members.txt'], check=True, timeout=300)
archive.unlink()
for tool in ('clang', 'clang++', 'llvm-symbolizer'):
    print(tool, Path('/opt/llvm/bin/' + tool).resolve(), flush=True)
    subprocess.run(['/opt/llvm/bin/' + tool, '--version'], check=True, timeout=10)
PY
FROM native-base
COPY --from=llvm-toolchain /opt/llvm /opt/llvm
COPY security/native/toolchain.json /opt/check/security/native/toolchain.json
COPY build/install-openssl.sh /opt/check/build/install-openssl.sh
RUN /opt/check/build/install-openssl.sh /opt/openssl-3.5.9
ENV PATH=/opt/llvm/bin:/usr/bin:/bin \
    OPENSSL_DIR=/opt/openssl-3.5.9 OPENSSL_STATIC=1 \
    PKG_CONFIG_PATH=/opt/openssl-3.5.9/lib/pkgconfig \
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
COPY vendor/mediasoup-sys-0.17.0 /opt/worker
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
COPY build/native_security.py build/security_codeql_resources.py build/security_tools.py /opt/check/build/
COPY ops/ansible/files/bounded_process.py ops/ansible/files/release_json.py /opt/check/ops/ansible/files/
COPY security/native /opt/check/security/native
RUN chmod -R a+rX /opt/check /opt/worker
WORKDIR /work
ENTRYPOINT ["/usr/bin/python3", "/opt/check/build/native_security.py", "_worker"]
