#!/usr/bin/env bash
# Start Docker only inside one privileged, disposable act job container.
set -euo pipefail
umask 077
[[ "${ACT:-}" == true && "${LOCAL_CI_DISPOSABLE:-}" == 1 ]] || {
  echo 'Private Docker setup is only for build/ci-local.sh runners.' >&2; exit 2;
}
[[ "$(uname -s)" == Linux && "$(uname -m)" =~ ^(x86_64|aarch64)$ ]] || {
  echo 'Private Docker requires a Linux AMD64 or ARM64 runner.' >&2; exit 2;
}
[[ -f /.dockerenv || -f /run/.containerenv ]] || {
  echo 'Refusing Docker setup outside a disposable job container.' >&2; exit 2;
}
[[ ! -e /var/run/docker.sock && ! -L /var/run/docker.sock ]] || {
  echo 'An existing Docker socket was left untouched; disable the outer socket mount.' >&2; exit 2;
}
for journal_path in /run/systemd/journal /run/log/journal /run/systemd/journald.conf.d/local-ci.conf; do
  [[ ! -e "$journal_path" && ! -L "$journal_path" ]] || {
    echo 'Existing journal state was left untouched; use a fresh disposable job container.' >&2; exit 2;
  }
done
[[ -x /lib/systemd/systemd-journald ]] || {
  echo 'The pinned act image must supply systemd-journald.' >&2; exit 2;
}
# The runner's embedded Moby build lags behind Docker's current release. Install
# the authenticated official static bundle only after the disposable-job guards.
# Runtime helpers are independently authenticated; never borrow a host daemon.
case "$(uname -m)" in
  x86_64)
    docker_arch=x86_64
    component_arch=amd64
    containerd_sha256=d65eda6a188aac1006848d8060099be88ba9c4bcddbfd9a23a961194710d0dd4
    buildx_sha256=982ca20490b45ed1ec8d99795974d3d874a358f75938c9c237305010e6b7e548
    docker_sha256=995d1ef289677f74fd58d8d2c35727b6a4ee389c69db8638a3e42d0487aa5b0f
    ;;
  aarch64)
    docker_arch=aarch64
    component_arch=arm64
    containerd_sha256=67f9b0a81c7140aaf15fe69053e88175270fadd33aeaffc1b9017123c1f55cea
    buildx_sha256=efa38cb7aa7db2dbb9ad049b00b0a9737f66f033626177b5a4e845184ad7ab29
    docker_sha256=76a624e4a8e5da654d1150e808175125efb5a6f1b6aa1cbd9caee18f51047a50
    ;;
esac
docker_tools="${RUNNER_TEMP:?}/local-ci-docker-tools"
mkdir "$docker_tools"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --retry 3 --max-time 120 \
  "https://download.docker.com/linux/static/stable/${docker_arch}/docker-29.8.2.tgz" \
  --output "$docker_tools/docker.tgz"
printf '%s  %s\n' "$docker_sha256" "$docker_tools/docker.tgz" | sha256sum --check --status
tar --extract --gzip --file "$docker_tools/docker.tgz" --directory "$docker_tools" \
  docker/containerd docker/containerd-shim-runc-v2 docker/ctr docker/docker \
  docker/docker-init docker/docker-proxy docker/dockerd docker/runc
sudo install -m 0755 "$docker_tools"/docker/* /usr/local/bin/
# Match the maintained host's current upstream components even when Docker's
# bundled containerd and the runner's Buildx package have not caught up yet.
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --retry 3 --max-time 120 \
  "https://github.com/containerd/containerd/releases/download/v2.4.1/containerd-2.4.1-linux-${component_arch}.tar.gz" \
  --output "$docker_tools/containerd.tgz"
printf '%s  %s\n' "$containerd_sha256" "$docker_tools/containerd.tgz" | sha256sum --check --status
tar --extract --gzip --file "$docker_tools/containerd.tgz" --directory "$docker_tools" \
  bin/containerd bin/containerd-shim-runc-v2 bin/ctr
sudo install -m 0755 "$docker_tools"/bin/* /usr/local/bin/
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --retry 3 --max-time 120 \
  "https://github.com/docker/buildx/releases/download/v0.37.2/buildx-v0.37.2.linux-${component_arch}" \
  --output "$docker_tools/docker-buildx"
printf '%s  %s\n' "$buildx_sha256" "$docker_tools/docker-buildx" | sha256sum --check --status
sudo install -d -m 0755 /usr/local/lib/docker/cli-plugins
sudo install -m 0755 "$docker_tools/docker-buildx" /usr/local/lib/docker/cli-plugins/docker-buildx
export PATH="/usr/local/bin:${PATH}"
[[ "$(dockerd --version)" == 'Docker version 29.8.2, build 8af9fe3' ]] || {
  echo 'The authenticated Docker daemon differs from the selected version.' >&2; exit 2;
}
[[ "$(docker buildx version)" == 'github.com/docker/buildx v0.37.2 '* ]] || {
  echo 'The installed Buildx plugin differs from the selected version.' >&2; exit 2;
}
# The launcher supplies a private cgroup namespace. Moby's hack/dind pattern
# moves its root processes into a leaf before delegating resource controllers.
if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
  mkdir -p /sys/fs/cgroup/init
  enabled=0
  for ((attempt = 0; attempt < 20; attempt++)); do
    while IFS= read -r process; do
      printf '%s\n' "$process" > /sys/fs/cgroup/init/cgroup.procs || true
    done < /sys/fs/cgroup/cgroup.procs
    if sed -e 's/ / +/g' -e 's/^/+/' < /sys/fs/cgroup/cgroup.controllers > /sys/fs/cgroup/cgroup.subtree_control; then
      enabled=1
      break
    fi
    sleep 0.05
  done
  [[ "$enabled" == 1 ]]
fi
docker_state="${RUNNER_TEMP:?}/local-ci-docker"
mkdir "$docker_state"
# Production Compose uses journald. Start the image's real daemon in this job's
# own filesystem; never mount a host journal or substitute a logging driver.
sudo mkdir -p /run/systemd/journald.conf.d
printf '%s\n' '[Journal]' 'Storage=volatile' 'RuntimeMaxUse=32M' 'ReadKMsg=no' |
  sudo tee /run/systemd/journald.conf.d/local-ci.conf > /dev/null
# Clear inherited socket activation and journal-directory overrides.
# shellcheck disable=SC2024
sudo env -i PATH="${PATH}" /lib/systemd/systemd-journald \
  > "$docker_state/journald.log" 2>&1 < /dev/null &
journal_pid=$!
printf '%s\n' "$journal_pid" > "$docker_state/journald.pid"
journal_ready=0
for ((attempt = 0; attempt < 30; attempt++)); do
  kill -0 "$journal_pid" 2>/dev/null || break
  if [[ -S /run/systemd/journal/socket ]]; then
    journal_ready=1
    break
  fi
  sleep 0.1
done
if [[ "$journal_ready" != 1 ]]; then
  kill -TERM "$journal_pid" 2>/dev/null || true
  tail -n 60 "$docker_state/journald.log" >&2
  echo 'The job-owned journal did not become ready.' >&2
  exit 1
fi
sudo mkdir -p /run/local-ci-docker
# Logs belong to the caller's private runner directory.
# shellcheck disable=SC2024
sudo env -u DOCKER_HOST -u DOCKER_CONTEXT PATH="${PATH}" dockerd \
  --host unix:///var/run/docker.sock --data-root /var/lib/local-ci-docker \
  --exec-root /run/local-ci-docker/exec --pidfile /run/local-ci-docker/daemon.pid \
  --storage-driver overlay2 --bip 172.30.0.1/24 \
  --default-address-pool base=172.31.0.0/16,size=24 \
  > "$docker_state/daemon.log" 2>&1 < /dev/null &
for ((attempt = 0; attempt < 30; attempt++)); do
  if timeout --signal=TERM --kill-after=2s 3s docker --host unix:///var/run/docker.sock info >/dev/null 2>&1; then
    if ! kill -0 "$journal_pid" 2>/dev/null || [[ ! -S /run/systemd/journal/socket ]]; then
      echo 'The job-owned journal exited during Docker startup.' >&2
      exit 1
    fi
    timeout --signal=TERM --kill-after=2s 10s docker --host unix:///var/run/docker.sock version --format '{{json .Server}}' > "$docker_state/server.json"
    python3 - "$docker_state/server.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as source:
    server = json.load(source)
if server.get("Version") != "29.8.2":
    raise SystemExit("The running Docker daemon differs from the selected version")
for name, expected in (("containerd", "2.4.1"), ("runc", "1.5.2")):
    versions = [item.get("Version", "").removeprefix("v") for item in server.get("Components", [])
                if item.get("Name") == name]
    if versions != [expected]:
        raise SystemExit("The running Docker component differs from the selected version")
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
