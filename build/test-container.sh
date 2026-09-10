#!/usr/bin/env bash
# Test an already-built image using only newly created, disposable containers.
set -euo pipefail
umask 077
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
production_image="${PRODUCTION_IMAGE:-simplestchat-ci:production}"
# Official multi-architecture manifest, verified 2026-09-09.
postgres_image='docker.io/library/postgres:18.6-bookworm@sha256:1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af'
docker info >/dev/null
docker image inspect "${production_image}" >/dev/null
test_artifacts="${CONTAINER_TEST_ARTIFACTS:-$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-container-tests.XXXXXX")}"
mkdir -p "${test_artifacts}"
database_id=''
server_id=''
cleanup() {
  test_status=$?
  trap - EXIT
  for container_id in "${server_id}" "${database_id}"; do
    if [[ -n "${container_id}" ]]; then
      docker logs "${container_id}" >"${test_artifacts}/${container_id}.log" 2>&1 || true
      if ((test_status != 0)); then
        tail -n 80 "${test_artifacts}/${container_id}.log" >&2 || true
      fi
      docker rm --force --volumes "${container_id}" >/dev/null || true
    fi
  done
  echo "Container logs: ${test_artifacts}; disposable containers and their test data removed."
  exit "${test_status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# PostgreSQL and the app share this new network namespace. Database traffic stays
# loopback (no production TLS exception); only HTTP is published to host loopback.
# PostgreSQL data lives in tmpfs and cannot touch an existing database/volume.
database_id="$(docker run --detach \
  --publish 127.0.0.1::3000 \
  --tmpfs /var/lib/postgresql:rw,nosuid,nodev,size=512m \
  --env POSTGRES_USER=test_owner \
  --env POSTGRES_PASSWORD=disposable-container-password \
  --env POSTGRES_DB=simplestchat_test \
  "${postgres_image}" postgres -c listen_addresses=127.0.0.1)"
database_ready=0
for ((attempt = 0; attempt < 120; attempt++)); do
  test "$(docker inspect --format '{{.State.Running}}' "${database_id}")" = true
  if docker exec "${database_id}" pg_isready -h 127.0.0.1 -U test_owner -d simplestchat_test >/dev/null 2>&1; then
    database_ready=1
    break
  fi
  sleep 0.5
done
test "${database_ready}" = 1
published_address="$(docker port "${database_id}" 3000/tcp)"
[[ "${published_address}" =~ ^127\.0\.0\.1:[0-9]+$ ]]
test_origin="http://${published_address}"

server_id="$(docker run --detach \
  --network "container:${database_id}" \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=16m \
  --env DATABASE_URL='postgres://test_owner:disposable-container-password@127.0.0.1:5432/simplestchat_test?sslmode=disable' \
  --env JWT_SECRET=disposable-container-jwt-secret-at-least-32-bytes \
  --env RUN_MIGRATIONS=true --env REGISTRATION_ENABLED=true \
  --env BIND_ADDR=0.0.0.0 --env PORT=3000 --env ANNOUNCE_IP=127.0.0.1 \
  --env ALLOWED_ORIGINS="${test_origin}" --env MEDIA_WORKERS=1 \
  "${production_image}")"
server_ready=0
for ((attempt = 0; attempt < 120; attempt++)); do
  test "$(docker inspect --format '{{.State.Running}}' "${server_id}")" = true
  if curl --fail --silent --max-time 2 "${test_origin}/api/rooms" >"${test_artifacts}/rooms.json"; then
    server_ready=1
    break
  fi
  sleep 0.5
done
test "${server_ready}" = 1
test "$(curl --fail --silent --show-error --max-time 5 "${test_origin}/health")" = '{"status":"ok"}'
curl --fail --silent --show-error --max-time 5 "${test_origin}/" >"${test_artifacts}/index.html"
grep -iq '<!doctype html>' "${test_artifacts}/index.html"
test "$(<"${test_artifacts}/rooms.json")" = '[]'

# Verify that startup ran every packaged migration, not just a liveness endpoint.
shopt -s nullglob
migrations=(migrations/*.sql)
applied="$(docker exec "${database_id}" psql -X -U test_owner -d simplestchat_test -At \
  -c 'SELECT COUNT(*) FROM _sqlx_migrations WHERE success')"
test "${applied}" -eq "${#migrations[@]}"
test "${applied}" -gt 0

# Exercise one real auth write without retaining the returned JWT or refresh cookie.
curl --fail --silent --show-error --max-time 15 \
  --header 'Content-Type: application/json' --header "Origin: ${test_origin}" \
  --data '{"email":"container-smoke@example.test","password":"Disposable-container-password-2026!","display_name":"Container smoke"}' \
  --output /dev/null "${test_origin}/api/auth/register"
registered="$(docker exec "${database_id}" psql -X -U test_owner -d simplestchat_test -At \
  -c "SELECT COUNT(*) FROM users WHERE email = 'container-smoke@example.test'")"
test "${registered}" = 1

# The same image must restart against its migrated database without rerunning DDL.
docker restart "${server_id}" >/dev/null
server_ready=0
for ((attempt = 0; attempt < 120; attempt++)); do
  test "$(docker inspect --format '{{.State.Running}}' "${server_id}")" = true
  if curl --fail --silent --max-time 2 "${test_origin}/api/rooms" >/dev/null; then
    server_ready=1
    break
  fi
  sleep 0.5
done
test "${server_ready}" = 1
echo "PASS production image: migrations (${applied}), static UI, health, DB-backed API, registration and restart."
