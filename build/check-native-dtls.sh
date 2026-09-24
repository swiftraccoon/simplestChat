#!/usr/bin/env bash
# Test the maintained native DTLS patch without changing Cargo's worker archive.
set -euo pipefail
umask 077

if [[ "$#" != 0 ]]; then
  echo 'Usage: build/check-native-dtls.sh' >&2
  exit 2
fi
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
dtls_python="${PYTHON:-python3}"
dtls_cc="${CC:-clang}"
dtls_cxx="${CXX:-clang++}"
dtls_openssl="${OPENSSL_DIR:-${project_root}/target/openssl-3.5.8}"
dtls_temp_parent="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
for dtls_tool in "${dtls_python}" "${dtls_cc}" "${dtls_cxx}" pkg-config; do
  if ! command -v "${dtls_tool}" >/dev/null; then
    echo "Missing native test tool: ${dtls_tool}" >&2
    exit 2
  fi
done
if [[ "${dtls_openssl}" != /* || ! -f "${dtls_openssl}/lib/pkgconfig/openssl.pc" ]]; then
  echo 'OPENSSL_DIR must name an absolute pinned OpenSSL installation (build/install-openssl.sh).' >&2
  exit 2
fi
if [[ "${dtls_temp_parent}" != /* || ! -d "${dtls_temp_parent}" ]]; then
  echo 'RUNNER_TEMP/TMPDIR must name an existing absolute temporary directory.' >&2
  exit 2
fi

dtls_temp="$(mktemp -d "${dtls_temp_parent%/}/simplestchat-native-dtls.XXXXXXXX")"
dtls_pid=''
cleanup() {
  dtls_status=$?
  trap - EXIT
  trap '' INT TERM
  if [[ -n "${dtls_pid}" ]] && kill -0 -- "-${dtls_pid}" 2>/dev/null; then
    # Only this helper's child job group is owned. Never signal by process name.
    kill -TERM -- "-${dtls_pid}" 2>/dev/null || true
    for ((attempt = 0; attempt < 50; attempt++)); do
      kill -0 -- "-${dtls_pid}" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 -- "-${dtls_pid}" 2>/dev/null; then
      kill -KILL -- "-${dtls_pid}" 2>/dev/null || true
    fi
    wait "${dtls_pid}" 2>/dev/null || true
  fi
  if ((dtls_status == 0)); then
    rm -rf -- "${dtls_temp}"
  else
    echo "Native DTLS test failed; logs retained in ${dtls_temp}" >&2
    for dtls_log in build.log dtls.log orderly-close.log; do
      if [[ -f "${dtls_temp}/${dtls_log}" ]]; then
        tail -n 60 "${dtls_temp}/${dtls_log}" >&2
      fi
    done
    # Retain diagnostic logs, not the large disposable build or tool trees.
    rm -rf -- "${dtls_temp}/worker" "${dtls_temp}/out" "${dtls_temp}/build" "${dtls_temp}/install"
  fi
  exit "${dtls_status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cp -R -- "${project_root}/vendor/mediasoup-sys-0.17.0" "${dtls_temp}/worker"
cat >"${dtls_temp}/run.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
"$PYTHON" -m pip install --no-user --target "$MEDIASOUP_OUT_DIR/pip_invoke" \
  --require-hashes --only-binary=:all: \
  --requirement "$1/worker/python-invoke-requirements.txt"
"$PYTHON" -m invoke --search-root "$1/worker" test > "$1/dtls.log" 2>&1
"$BUILD_DIR/mediasoup-worker-test" '[dtls-close]' > "$1/orderly-close.log" 2>&1
"$PYTHON" - "$1/orderly-close.log" <<'PY'
import pathlib
import sys

output = pathlib.Path(sys.argv[1]).read_text()
if "RTC::DtlsTransport::" in output:
    raise SystemExit("Orderly authenticated DTLS close unexpectedly logged a warning/error")
PY
SH
bash -n "${dtls_temp}/run.sh"
echo 'Building isolated native DTLS regressions with pinned OpenSSL and Python tools...'

# Do not inherit Python modules, pip configuration, Meson options or test tags.
# The fixed source snapshot and reviewed wheel hashes define this test build.
set -m
env -i PATH="${PATH}" \
  PYTHON="${dtls_python}" PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1 \
  PYTHONPATH="${dtls_temp}/out/pip_invoke" \
  PIP_CONFIG_FILE=/dev/null PIP_INDEX_URL=https://pypi.org/simple \
  PIP_CONSTRAINT="${project_root}/build/pip-constraints.txt" \
  PKG_CONFIG_PATH="${dtls_openssl}/lib/pkgconfig" \
  CC="${dtls_cc}" CXX="${dtls_cxx}" \
  MEDIASOUP_OUT_DIR="${dtls_temp}/out" \
  MEDIASOUP_INSTALL_DIR="${dtls_temp}/install" BUILD_DIR="${dtls_temp}/build" \
  MEDIASOUP_TEST_TAGS='[dtls]' MS_TEST_LOG_LEVEL=warn MS_TEST_LOG_TAGS=dtls \
  bash "${dtls_temp}/run.sh" "${dtls_temp}" >"${dtls_temp}/build.log" 2>&1 &
dtls_pid=$!
set +m
wait "${dtls_pid}"
dtls_pid=''
tail -n 3 "${dtls_temp}/dtls.log"
echo 'Authenticated orderly DTLS close emitted no warning/error logs.'
