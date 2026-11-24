#!/usr/bin/env bash
# Preserve loopback-only database guards with act's published service ports.
set -euo pipefail
umask 077
[[ "${ACT:-}" == true && "${LOCAL_CI_DISPOSABLE:-}" == 1 ]] || {
  echo 'The PostgreSQL relay is only for disposable local CI runners.' >&2; exit 2;
}
[[ "$(uname -s)" == Linux && ( -f /.dockerenv || -f /run/.containerenv ) ]] || {
  echo 'Refusing the PostgreSQL relay outside a disposable job container.' >&2; exit 2;
}
[[ "${CI_LOCAL_POSTGRES_PORT:-}" =~ ^543[2-5]$ ]] || {
  echo 'The local PostgreSQL service must use its declared CI port.' >&2; exit 2;
}
relay_state="${RUNNER_TEMP:?}/local-ci-postgres"
mkdir "$relay_state"
relay_ready="$relay_state/ready"
# The relay is this checkout's CI binary; it is built before the readiness wait
# starts, then relays until the job container ends.
cargo build --locked -q -p simplestchat-ci --bin simplestchat-ci
cargo run --locked -q -p simplestchat-ci -- local-postgres-relay \
  "$CI_LOCAL_POSTGRES_PORT" "$relay_ready" > "$relay_state/relay.log" 2>&1 < /dev/null &
relay_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
  if [[ -f "$relay_ready" ]]; then
    printf 'Started the job-local PostgreSQL relay on port %s.\n' "$CI_LOCAL_POSTGRES_PORT"
    exit 0
  fi
  kill -0 "$relay_pid" 2>/dev/null || break
  sleep 0.1
done
cat "$relay_state/relay.log" >&2
echo 'The job-local PostgreSQL relay did not become ready.' >&2
exit 1
