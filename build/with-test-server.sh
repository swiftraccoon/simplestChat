#!/usr/bin/env bash
# Run a command against an owned HTTP-loopback server and a disposable database.
# Example: DISPOSABLE_TEST_DATABASE=1 DATABASE_URL=postgres://.../chat_test \
#   build/with-test-server.sh npm --prefix web/e2e test
set -euo pipefail
umask 077

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if [[ "${DISPOSABLE_TEST_DATABASE:-}" != 1 || "$#" -eq 0 ]]; then
  echo 'Set DISPOSABLE_TEST_DATABASE=1 and pass the test command to run.' >&2
  exit 2
fi
# Reject remote hosts, query-string host overrides, and non-test database names.
database_pattern='^postgres(ql)?://([^/@]+@)?(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?/[A-Za-z0-9_]+_test(\?sslmode=disable)?$'
if [[ ! "${DATABASE_URL:-}" =~ ${database_pattern} ]]; then
  echo 'DATABASE_URL must use loopback and a name ending _test; only sslmode=disable is accepted as a query.' >&2
  exit 2
fi

server_binary="${TEST_SERVER_BINARY:-${repo_root}/target/debug/simplestChat}"
server_workdir="${TEST_SERVER_WORKDIR:-${repo_root}}"
test -f "${server_workdir}/Cargo.toml"
test -d "${server_workdir}/migrations"
test_port="${TEST_SERVER_PORT:-3119}"
media_port="${TEST_MEDIA_PORT:-41010}"
# An explicitly empty override is invalid, not a request for the default.
test_announce_ip="${TEST_ANNOUNCE_IP-127.0.0.1}"
for port in "${test_port}" "${media_port}"; do
  if [[ ! "${port}" =~ ^[1-9][0-9]{3,4}$ ]] || ((port > 65535)); then
    echo 'Test ports must be integers from 1000 through 65535.' >&2
    exit 2
  fi
done
test -x "${server_binary}"
export BASE_URL="http://127.0.0.1:${test_port}"
# Probe both sockets; a non-HTTP listener must not be mistaken for a free port.
# The server still owns the final bind and must remain alive after readiness.
node --input-type=module - "${test_port}" "${media_port}" "${test_announce_ip}" <<'JS'
import net from 'node:net';
import dgram from 'node:dgram';
import os from 'node:os';
const announceIp = process.argv[4];
try {
  if (!net.isIPv4(announceIp)) throw new Error('expected a literal IPv4 address');
  if (announceIp !== '127.0.0.1' && !Object.values(os.networkInterfaces()).some(addresses =>
    addresses?.some(address => address.address === announceIp))) {
    throw new Error('address is not assigned to a local network interface');
  }
} catch (error) {
  console.error(`Refusing TEST_ANNOUNCE_IP: ${error.message}`);
  process.exit(2);
}
const tcp = net.createServer();
const udp = dgram.createSocket('udp4');
try {
  await new Promise((resolve, reject) => {
    tcp.once('error', reject).listen(Number(process.argv[2]), '127.0.0.1', resolve);
  });
  await new Promise((resolve, reject) => {
    udp.once('error', reject).bind(Number(process.argv[3]), '0.0.0.0', resolve);
  });
} catch (error) {
  console.error(`Refusing unavailable test ports: ${error.message}`);
  process.exitCode = 2;
} finally {
  tcp.close();
  try { udp.close(); } catch {}
}
JS
export TEST_ANNOUNCE_IP="${test_announce_ip}"

test_artifacts="${E2E_ARTIFACTS:-$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-tests.XXXXXX")}"
mkdir -p "${test_artifacts}"
export E2E_ARTIFACTS="${test_artifacts}"
server_pid=''
cleanup() {
  test_status=$?
  trap - EXIT
  trap '' INT TERM
  term_attempted=false
  term_sent=false
  kill_attempted=false
  kill_sent=false
  wait_status=''
  if [[ -n "${server_pid}" ]]; then
    term_attempted=true
    if kill -TERM "${server_pid}" 2>/dev/null; then term_sent=true; fi
    # Bound shutdown; never kill by executable name or target another server.
    for ((attempt = 0; attempt < 50; attempt++)); do
      kill -0 "${server_pid}" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "${server_pid}" 2>/dev/null; then
      kill_attempted=true
      if kill -KILL "${server_pid}" 2>/dev/null; then kill_sent=true; fi
    fi
    if wait "${server_pid}" 2>/dev/null; then wait_status=0; else wait_status=$?; fi
  fi
  # Record observed wait status separately from requested signals. Bash cannot
  # distinguish signal termination from an explicit high exit code (or exit 127
  # from unavailable child status), so retain that ambiguity rather than infer it.
  if ! env -i PATH="${PATH}" node --input-type=module - "${test_artifacts}" \
    "${server_pid}" "${test_status}" "${term_attempted}" "${term_sent}" \
    "${kill_attempted}" "${kill_sent}" "${wait_status}" <<'JS'
import fs from 'node:fs';
import path from 'node:path';
const [artifacts, pid, status, termAttempted, termSent, killAttempted, killSent, waited] = process.argv.slice(2);
const waitStatus = waited === '' ? null : Number(waited);
const passed = waitStatus === 0 && killAttempted === 'false';
const report = {
  schemaVersion: 1,
  serverPid: pid === '' ? null : Number(pid),
  statusBeforeCleanup: Number(status),
  termAttempted: termAttempted === 'true',
  termSent: termSent === 'true',
  killAttempted: killAttempted === 'true',
  killSent: killSent === 'true',
  waitStatus,
  waitStatusInterpretation: waitStatus === null ? 'not_waited'
    : waitStatus === 0 ? 'exited_zero'
      : waitStatus === 127 ? 'unavailable_or_exit_127'
        : waitStatus >= 128 ? 'signal_or_high_exit_code' : 'nonzero_exit',
  passed,
  finishedAt: new Date().toISOString(),
};
try {
  // A fresh report is required; never overwrite an earlier run's evidence.
  fs.writeFileSync(path.join(artifacts, 'server-shutdown.json'), `${JSON.stringify(report, null, 2)}\n`, {
    flag: 'wx', mode: 0o600,
  });
  if (!passed) process.exitCode = 1;
} catch {
  console.error('Could not retain the owned test server shutdown report.');
  process.exitCode = 1;
}
JS
  then
    echo 'Owned test server shutdown was unsuccessful or its evidence was unavailable.' >&2
    if ((test_status == 0)); then test_status=1; fi
  fi
  echo "Test server log and browser artifacts: ${test_artifacts}"
  if ((test_status != 0)); then
    tail -n 80 "${test_artifacts}/server.log" >&2 || true
  fi
  exit "${test_status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Only the lifecycle gate needs protected server counts. Never inherit an
# operator credential or publish this disposable token in logs/artifacts.
test_metrics_environment=("PATH=${PATH}")
unset TEST_METRICS_TOKEN TEST_SERVER_PID
if [[ "${LIFECYCLE_E2E:-}" == 1 ]]; then
  TEST_METRICS_TOKEN="$(env -i PATH="${PATH}" node -e 'process.stdout.write(require("node:crypto").randomBytes(32).toString("hex"))')"
  export TEST_METRICS_TOKEN
  test_metrics_environment+=("METRICS_TOKEN=${TEST_METRICS_TOKEN}")
fi

# A clean server environment also prevents inherited rate limits, TURN, passkey,
# proxy, or account settings from changing the workload. Keep normal defaults.
(
  cd "${server_workdir}"
  exec env -i "${test_metrics_environment[@]}" DATABASE_URL="${DATABASE_URL}" \
  BIND_ADDR=127.0.0.1 PORT="${test_port}" ANNOUNCE_IP="${test_announce_ip}" \
  MEDIA_WORKERS=1 WEBRTC_SERVER_PORT_BASE="${media_port}" \
  ALLOWED_ORIGINS="${BASE_URL}" REGISTRATION_ENABLED=true ALLOW_AD_HOC_ROOMS=true \
  JWT_SECRET=disposable-test-only-jwt-secret-at-least-32-bytes \
    RUN_MIGRATIONS=true "${server_binary}"
) >"${test_artifacts}/server.log" 2>&1 &
server_pid=$!
export TEST_SERVER_PID="${server_pid}"
ready=0
for ((attempt = 0; attempt < 120; attempt++)); do
  if ! kill -0 "${server_pid}" 2>/dev/null; then
    echo 'Test server exited before readiness.' >&2
    exit 1
  fi
  if curl --fail --silent --max-time 2 "${BASE_URL}/api/rooms" >/dev/null; then
    kill -0 "${server_pid}" 2>/dev/null
    ready=1
    break
  fi
  sleep 0.25
done
if [[ "${ready}" != 1 ]]; then
  echo 'Test server failed database-backed API readiness.' >&2
  exit 1
fi
kill -0 "${server_pid}" 2>/dev/null

export COMMUNITY_E2E=1
export TEST_DATABASE_URL="${DATABASE_URL}"
"$@"
if ! kill -0 "${server_pid}" 2>/dev/null; then
  echo 'Test server exited while the test command was running.' >&2
  exit 1
fi
