#!/usr/bin/env bash
# Explicit, bounded maintenance deployment. Never deletes database/certificate data.
set -euo pipefail
umask 077
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
unset DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_CERT_PATH DOCKER_CONFIG
export DOCKER_HOST=unix:///var/run/docker.sock
[[ $(id -u) == 0 && $# == 0 ]]
config=/etc/simplestchat-public
root=/srv/simplestchat-public
[[ ! -L "$config" && $(stat -c '%u:%a' "$config") == 0:700 ]]
[[ ! -L "$root" && $(stat -c '%u:%a' "$root") == 0:700 ]]
if [[ ! -e /run/simplestchat-bench ]]; then mkdir -m 700 /run/simplestchat-bench; fi
[[ ! -L /run/simplestchat-bench && $(stat -c '%u:%a' /run/simplestchat-bench) == 0:700 ]]
[[ ! -L /run/simplestchat-bench/workload.lock ]]
exec 9<>/run/simplestchat-bench/workload.lock
[[ $(stat -c '%u:%a' /run/simplestchat-bench/workload.lock) == 0:600 ]]
flock -n 9 || { echo 'A benchmark, image build or deployment is active.' >&2; exit 1; }
if [[ -e /run/simplestchat-bench/current.json ]]; then
  jq -e '.schemaVersion == 1 and .finalized == true' /run/simplestchat-bench/current.json >/dev/null
fi
compose() { timeout --signal=TERM --kill-after=5s 180s /usr/local/bin/simplestchat-public "$@"; }
docker_owned() { timeout --signal=TERM --kill-after=2s 15s docker "$@"; }
# Deployment may retain an already-running private DB, but never migrates under
# a live application or edits an unattended previous maintenance container.
for service in simplestchat caddy; do
  [[ -z $(compose ps --status running --quiet "$service") ]] || {
    echo 'Stop the public application and proxy before maintenance.' >&2; exit 1;
  }
done
[[ -z $(compose --profile maintenance ps --all --quiet migrate) ]] || {
  echo 'Inspect the retained migration container before another deployment.' >&2; exit 1;
}
attempt=$(mktemp -d "$root/results/deploy.XXXXXXXX")
run_id=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
server_image=$(jq -er '.serverImage | select(test("^sha256:[a-f0-9]{64}$"))' "$config/images.json")
caddy_image=$(jq -er .caddyImage "$config/images.json")
revision=$(jq -er '.revision | select(test("^[a-f0-9]{40}$"))' "$config/images.json")
started=$(date -u +%FT%TZ)
phase=configuration
validation_id='' migration_id=''
validation_name="scpub-$run_id-validate"
finish() {
  status=$?
  trap - EXIT INT TERM
  if [[ "$status" != 0 ]]; then
    # Keep failed deployments private; preserve stopped containers and all data.
    compose --profile maintenance stop caddy simplestchat migrate >"$attempt/failure-stop.log" 2>&1 || true
  fi
  if [[ -n "$validation_id" ]]; then
    docker_owned logs "$validation_id" >"$attempt/caddy-validation.log" 2>&1 || true
    if [[ $(docker_owned inspect --format '{{.State.Running}}' "$validation_id") == true ]]; then
      timeout 10s docker stop --time 5 "$validation_id" >"$attempt/validation-stop.log" 2>&1 || true
    fi
    # A failed validation container is retained for inspection, not force-removed.
  fi
  compose --profile maintenance ps --all --format json >"$attempt/containers.json" 2>"$attempt/container-state-error.log" || true
  compose --profile maintenance logs --no-color --tail 150 simplestchat postgres caddy migrate >"$attempt/services.log" 2>&1 || true
  jq -n --arg started "$started" --arg finished "$(date -u +%FT%TZ)" --arg phase "$phase" \
    --argjson status "$status" --arg image "$server_image" \
    '{startedAt:$started,finishedAt:$finished,phase:$phase,exitStatus:$status,passed:($status==0),serverImage:$image}' >"$attempt/outcome.json"
  echo "Public deployment evidence: $attempt (exit=$status)"
  exit "$status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cp "$config/images.json" "$attempt/images.json"
sha256sum "$0" /usr/local/bin/simplestchat-public "$config/compose.base.yml" "$config/compose.public.yml" "$config/Caddyfile" >"$attempt/deployment-inputs.sha256"
compose config --quiet
[[ $(docker_owned image inspect --format '{{.Id}}' "$server_image") == "$server_image" ]]
[[ $(docker_owned image inspect --format '{{.Config.User}}' "$server_image") == 10001:10001 ]]
[[ $(docker_owned image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$server_image") == "$revision" ]]
source_root="/srv/simplestchat-bench/sources/$revision"
[[ $(git -C "$source_root" rev-parse HEAD) == "$revision" ]]
[[ -z $(git -C "$source_root" status --porcelain --untracked-files=all) ]]

phase=proxy_validation
printf '%s\n' "$validation_name" >"$attempt/validation-name.txt"
# A named, unpublished validator cannot serve ACME/HTTP traffic or contact peers.
docker_owned create --name "$validation_name" --cidfile "$attempt/validation.cid" \
  --label "simplestchat.public.validation=$run_id" --network none --pull never \
  --read-only --user 10001:10001 --cap-drop ALL --cap-add NET_BIND_SERVICE --security-opt no-new-privileges \
  --memory 256m --pids-limit 128 --log-driver local --log-opt max-size=10m --log-opt max-file=3 \
  --env-file "$config/proxy.env" --mount "type=bind,src=$config/Caddyfile,dst=/etc/caddy/Caddyfile,readonly" \
  --tmpfs /data:rw,uid=10001,gid=10001,size=8m --tmpfs /config:rw,uid=10001,gid=10001,size=8m \
  "$caddy_image" caddy validate --config /etc/caddy/Caddyfile >"$attempt/validation-create.log"
validation_id=$(<"$attempt/validation.cid")
docker_owned start "$validation_id" >"$attempt/validation-start.log"
timeout --signal=TERM --kill-after=2s 30s docker wait "$validation_id" >"$attempt/validation-exit.txt"
[[ $(<"$attempt/validation-exit.txt") == 0 ]]
docker_owned logs "$validation_id" >"$attempt/caddy-validation.log" 2>&1
docker_owned rm "$validation_id" >"$attempt/validation-remove.log"
validation_id=''

phase=database
compose up --detach --no-build --pull never --wait --wait-timeout 120 postgres >"$attempt/database-start.log" 2>&1
database_id=$(compose ps --quiet postgres)
[[ "$database_id" =~ ^[a-f0-9]{64}$ ]]
[[ $(docker_owned exec --user 999:999 "$database_id" id -un) == postgres ]]
psql_owned() {
  timeout --signal=TERM --kill-after=2s 20s docker exec --interactive --user 999:999 "$database_id" \
    psql --no-psqlrc --set ON_ERROR_STOP=on --host /run/simplestchat-postgres --username postgres --dbname simplestchat "$@"
}
phase=migration
compose --profile maintenance up --detach --no-build --pull never --no-deps migrate >"$attempt/migration-start.log" 2>&1
migration_id=$(compose --profile maintenance ps --quiet migrate)
[[ "$migration_id" =~ ^[a-f0-9]{64}$ ]]
[[ $(docker_owned inspect --format '{{.Image}}' "$migration_id") == "$server_image" ]]
deadline=$((SECONDS + 90))
while true; do
  pid=$(docker_owned inspect --format '{{.State.Pid}}' "$migration_id")
  [[ "$pid" =~ ^[1-9][0-9]*$ && "$pid" -gt 1 ]]
  exec {net_fd}<"/proc/$pid/ns/net"
  [[ $(docker_owned inspect --format '{{.State.Pid}}' "$migration_id") == "$pid" ]]
  ready=false
  if timeout 4s nsenter --net="/proc/self/fd/$net_fd" -- curl --disable --noproxy '*' --proto '=http' \
    --fail --silent --max-time 2 http://127.0.0.1:3000/ready >"$attempt/migration-ready.json"; then ready=true; fi
  exec {net_fd}<&-
  [[ "$ready" == true ]] && break
  ((SECONDS < deadline)); sleep 1
done
jq -e '.status == "ready"' "$attempt/migration-ready.json" >/dev/null
psql_owned --tuples-only --no-align --field-separator ' ' \
  --command "SELECT version, success, encode(checksum, 'hex') FROM public._sqlx_migrations ORDER BY version" >"$attempt/migration-ledger.txt"
python3 - "$revision" "$attempt/migration-ledger.txt" <<'PY'
import hashlib
from pathlib import Path
import sys
source = Path('/srv/simplestchat-bench/sources') / sys.argv[1] / 'migrations'
expected = {int(path.name.split('_')[0]): hashlib.sha384(path.read_bytes()).hexdigest() for path in source.glob('*.sql')}
actual = {}
for line in Path(sys.argv[2]).read_text().splitlines():
    version, success, checksum = line.split()
    assert success == 't' and int(version) not in actual
    actual[int(version)] = checksum
assert expected and actual == expected, 'Migration ledger differs from the verified source image'
print('Packaged migration checksums match.')
PY
compose --profile maintenance stop --timeout 30 migrate >"$attempt/migration-stop.log" 2>&1
docker_owned inspect --format '{{json .State}}' "$migration_id" >"$attempt/migration-state.json"
jq -e '.Running == false and .ExitCode == 0 and .OOMKilled == false and .Error == ""' "$attempt/migration-state.json" >/dev/null
docker_owned logs "$migration_id" >"$attempt/migration.log" 2>&1
docker_owned rm "$migration_id" >"$attempt/migration-remove.log"
migration_id=''
psql_owned <"$config/runtime-grants.sql" >"$attempt/runtime-grants.log"
psql_owned --tuples-only --no-align --command "SELECT NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls AND NOT has_database_privilege('simplestchat_app','simplestchat','CREATE') AND NOT has_schema_privilege('simplestchat_app','public','CREATE') AND NOT has_table_privilege('simplestchat_app','public._sqlx_migrations','SELECT,INSERT,UPDATE,DELETE') FROM pg_roles WHERE rolname='simplestchat_app'" >"$attempt/runtime-privileges.txt"
[[ $(<"$attempt/runtime-privileges.txt") == t ]]

phase=private_application
# Enrollment is briefly available only on the private backend for initial owner
# creation. The public proxy remains stopped; restore inventory policy below.
REGISTRATION_ENABLED=true compose up --detach --no-build --pull never simplestchat >"$attempt/application-start.log" 2>&1
deadline=$((SECONDS + 60))
until curl --disable --noproxy '*' --proto '=http' --fail --silent --max-time 2 http://127.0.0.1:3000/ready >"$attempt/application-ready.json"; do
  ((SECONDS < deadline)); sleep 1
done
jq -e '.status == "ready"' "$attempt/application-ready.json" >/dev/null
phase=lobby
timeout --signal=TERM --kill-after=2s 90s python3 /usr/local/libexec/simplestchat-bench/seed-public-room.py >"$attempt/lobby.json"
jq -e '.passed == true' "$attempt/lobby.json" >/dev/null
phase=runtime_policy
compose up --detach --no-build --pull never simplestchat >"$attempt/runtime-policy.log" 2>&1
deadline=$((SECONDS + 60))
until curl --disable --noproxy '*' --proto '=http' --fail --silent --max-time 2 http://127.0.0.1:3000/ready >"$attempt/runtime-ready.json"; do
  ((SECONDS < deadline)); sleep 1
done
jq -e '.status == "ready"' "$attempt/runtime-ready.json" >/dev/null
phase=backup
backup=$(mktemp "$root/backups/initial.XXXXXXXX.dump")
timeout --signal=TERM --kill-after=2s 60s docker exec --user 999:999 "$database_id" \
  pg_dump --host /run/simplestchat-postgres --username postgres --dbname simplestchat --format custom >"$backup"
[[ -s "$backup" ]]
sha256sum "$backup" >"$attempt/backup.sha256"
phase=public_proxy
compose up --detach --no-build --pull never caddy >"$attempt/proxy-start.log" 2>&1
phase=complete
echo 'Public services started. Verify trusted HTTPS and external media separately.'
