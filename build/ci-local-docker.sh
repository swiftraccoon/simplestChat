#!/usr/bin/env bash
# Start Docker only inside one privileged, disposable act job container.
set -euo pipefail
umask 077
[[ "${ACT:-}" == true && "${LOCAL_CI_DISPOSABLE:-}" == 1 ]] || {
  echo 'Private Docker setup is only for build/ci-local.sh runners.' >&2; exit 2;
}
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || {
  echo 'The local production gate requires Linux/amd64.' >&2; exit 2;
}
[[ -f /.dockerenv || -f /run/.containerenv ]] || {
  echo 'Refusing Docker setup outside a disposable job container.' >&2; exit 2;
}
[[ ! -e /var/run/docker.sock && ! -L /var/run/docker.sock ]] || {
  echo 'An existing Docker socket was left untouched; disable the outer socket mount.' >&2; exit 2;
}
# Both architecture-specific runner-image digests include this exact Moby build.
# Never install a floating replacement or fall back to the host engine.
[[ "$(dockerd --version)" == 'Docker version 29.7.2-1, build 6a43e3d5afddf4111da0f864bbc7cae5d7e95001' ]] || {
  echo 'The pinned act image must supply the expected Docker daemon.' >&2; exit 2;
}
docker_state="${RUNNER_TEMP:?}/local-ci-docker"
mkdir "$docker_state"
sudo mkdir -p /run/local-ci-docker
# Logs belong to the caller's private runner directory.
# shellcheck disable=SC2024
sudo env -u DOCKER_HOST -u DOCKER_CONTEXT dockerd \
  --host unix:///var/run/docker.sock --data-root /var/lib/local-ci-docker \
  --exec-root /run/local-ci-docker/exec --pidfile /run/local-ci-docker/daemon.pid \
  --storage-driver overlay2 --bip 172.30.0.1/24 \
  --default-address-pool base=172.31.0.0/16,size=24 \
  > "$docker_state/daemon.log" 2>&1 < /dev/null &
for ((attempt = 0; attempt < 30; attempt++)); do
  if timeout --signal=TERM --kill-after=2s 3s docker --host unix:///var/run/docker.sock info >/dev/null 2>&1; then
    timeout --signal=TERM --kill-after=2s 10s docker --host unix:///var/run/docker.sock version --format '{{json .Server}}' > "$docker_state/server.json"
    python3 - "$docker_state/server.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as source:
    server = json.load(source)
if tuple(map(int, server["ApiVersion"].split("."))) < (1, 48):
    raise SystemExit("Docker API 1.48 or newer is required")
PY
    printf '%s\n' 'DOCKER_HOST=unix:///var/run/docker.sock' 'DOCKER_CONTEXT=' >> "${GITHUB_ENV:?}"
    echo 'Started the job-owned Docker daemon; the outer engine is not mounted.'
    exit 0
  fi
  sleep 0.5
done
tail -n 60 "$docker_state/daemon.log" >&2
echo 'The job-owned Docker daemon did not become ready.' >&2
exit 1
