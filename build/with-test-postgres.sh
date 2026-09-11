#!/usr/bin/env bash
# Run a command with a fresh, owned PostgreSQL database; never reuse services.
# Example: build/with-test-postgres.sh build/with-test-server.sh npm --prefix web/e2e test
set -euo pipefail
umask 077

if [[ "$#" -eq 0 ]]; then
  echo 'Usage: build/with-test-postgres.sh COMMAND [ARG ...]' >&2
  exit 2
fi
postgres_port="${TEST_POSTGRES_PORT-15434}"
if [[ ! "${postgres_port}" =~ ^[1-9][0-9]{3,4}$ ]] || ((postgres_port > 65535)); then
  echo 'TEST_POSTGRES_PORT must be an integer from 1000 through 65535.' >&2
  exit 2
fi
for postgres_tool in initdb pg_ctl createdb node; do
  if ! command -v "${postgres_tool}" >/dev/null; then
    echo "Missing ${postgres_tool}; put the PostgreSQL tools and Node on PATH." >&2
    exit 2
  fi
done

# The real server still owns its final bind; it cannot fall back to a Unix socket.
env -i PATH="${PATH}" node --input-type=module - "${postgres_port}" <<'JS'
import net from 'node:net';
const probe = net.createServer();
try {
  await new Promise((resolve, reject) => {
    probe.once('error', reject).listen(Number(process.argv[2]), '127.0.0.1', resolve);
  });
} catch (error) {
  console.error(`Refusing unavailable PostgreSQL test port: ${error.message}`);
  process.exitCode = 2;
} finally {
  probe.close();
}
JS

postgres_temp_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if [[ "${postgres_temp_root}" != /* || ! -d "${postgres_temp_root}" ]]; then
  echo 'RUNNER_TEMP/TMPDIR must identify an existing absolute temporary directory.' >&2
  exit 2
fi
postgres_cluster="$(mktemp -d "${postgres_temp_root%/}/simplestchat-postgres.XXXXXXXX")"
postgres_data="${postgres_cluster}/data"
postgres_start_attempted=0
test_pid=''

postgres_command() {
  # Ignore all PG*, service files, user configuration and inherited locale.
  env -i PATH="${PATH}" LC_ALL=C "$@"
}

cleanup() {
  test_status=$?
  trap - EXIT
  trap '' INT TERM
  if [[ -n "${test_pid}" ]]; then
    # The test command has its own job group, including nested helper children.
    # Never signal the caller's group or select processes by executable name.
    kill -TERM -- "-${test_pid}" 2>/dev/null || true
    for ((attempt = 0; attempt < 100; attempt++)); do
      kill -0 -- "-${test_pid}" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 -- "-${test_pid}" 2>/dev/null; then
      kill -KILL -- "-${test_pid}" 2>/dev/null || true
    fi
    wait "${test_pid}" 2>/dev/null || true
  fi
  if [[ "${postgres_start_attempted}" == 1 ]] && postgres_command pg_ctl -D "${postgres_data}" status >/dev/null 2>&1; then
    if ! postgres_command pg_ctl -D "${postgres_data}" -w -t 15 stop -m fast >>"${postgres_cluster}/postgres.log" 2>&1; then
      echo 'Fast shutdown failed; stopping only the owned test cluster immediately.' >&2
      if ((test_status == 0)); then test_status=1; fi
      postgres_command pg_ctl -D "${postgres_data}" -w -t 5 stop -m immediate >>"${postgres_cluster}/postgres.log" 2>&1 ||
        echo "Could not stop owned test cluster: ${postgres_data}" >&2
    fi
  fi
  echo "Disposable PostgreSQL logs: ${postgres_cluster}/{initdb,postgres}.log"
  if ((test_status != 0)); then
    tail -n 40 "${postgres_cluster}/initdb.log" "${postgres_cluster}/postgres.log" >&2 2>/dev/null || true
  fi
  # Retain the private directory for diagnostics; never delete user data here.
  exit "${test_status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

postgres_command initdb -D "${postgres_data}" -U test_owner \
  --auth-local=reject --auth-host=trust --encoding=UTF8 --no-locale >"${postgres_cluster}/initdb.log" 2>&1
postgres_start_attempted=1
postgres_command pg_ctl -D "${postgres_data}" -l "${postgres_cluster}/postgres.log" -w -t 15 \
  -o "-h 127.0.0.1 -p ${postgres_port} -c unix_socket_directories=''" start >>"${postgres_cluster}/initdb.log" 2>&1
postgres_command createdb -h 127.0.0.1 -p "${postgres_port}" -U test_owner \
  --maintenance-db=postgres simplestchat_test >>"${postgres_cluster}/initdb.log" 2>&1
postgres_command pg_ctl -D "${postgres_data}" status >/dev/null

# Prevent libpq defaults from redirecting the child to an inherited service.
for postgres_variable in ${!PG@}; do unset "${postgres_variable}"; done
export DATABASE_URL="postgres://test_owner@127.0.0.1:${postgres_port}/simplestchat_test?sslmode=disable"
export TEST_DATABASE_URL="${DATABASE_URL}"
export DISPOSABLE_TEST_DATABASE=1

# Job control gives this one command an owned process group on macOS and Linux.
# Waiting asynchronously lets signal traps run while the child is still active.
set -m
"$@" &
test_pid=$!
set +m
wait "${test_pid}"
if ! postgres_command pg_ctl -D "${postgres_data}" status >/dev/null 2>&1; then
  echo 'Owned PostgreSQL exited while the test command was running.' >&2
  exit 1
fi
