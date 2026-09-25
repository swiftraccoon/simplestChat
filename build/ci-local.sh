#!/usr/bin/env bash
# Run one job of .github/workflows/ci.yml locally with act inside the Podman machine.
#
#   build/ci-local.sh <job> [act options...]
#
#   build/ci-local.sh web
#   build/ci-local.sh rust-lint
#   build/ci-local.sh browser --matrix group:accounts
#   build/ci-local.sh native-dtls
#
# The job runs in the act Ubuntu 24.04 image on this machine's architecture
# (arm64 on Apple silicon), so it validates workflow logic, Linux-only browser
# behaviour and step wiring; its timings do not transfer to the x86 runners.
# The workspace is copied into the container honouring .gitignore, so local
# build trees stay out. actions/cache and artifacts persist under target/act.
# The deployment job needs the runner's Docker engine and is not supported here.
set -euo pipefail

if [[ "$#" -lt 1 || "$1" == -* ]]; then
  echo 'Usage: build/ci-local.sh <job> [act options...]' >&2
  exit 2
fi
job="$1"
shift

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
for tool in act curl; do
  if ! command -v "${tool}" >/dev/null; then
    echo "Missing ${tool}: brew install ${tool}" >&2
    exit 2
  fi
done

engine_ready() {
  curl --silent --fail --max-time 3 --unix-socket "$1" http://localhost/_ping >/dev/null 2>&1
}

# act speaks the Docker API. Prefer an explicit DOCKER_HOST, then a running
# Docker Desktop, then a Podman machine owned by this script. The default
# Podman machine is not used: its Docker-compatible container listing fails
# here with "getting graph driver info ... invalid argument" (a stale record
# from another project), which stops act before the job starts.
machine=simplestchat-ci
if [[ -z "${DOCKER_HOST:-}" ]]; then
  desktop_socket="${HOME}/.docker/run/docker.sock"
  if engine_ready "${desktop_socket}"; then
    export DOCKER_HOST="unix://${desktop_socket}"
  else
    if ! command -v podman >/dev/null; then
      echo 'No container engine: start Docker Desktop or install Podman (brew install podman).' >&2
      exit 2
    fi
    if ! podman machine inspect "${machine}" >/dev/null 2>&1; then
      echo "Creating the ${machine} Podman machine (rootful, 6 CPUs, 12 GiB)..."
      podman machine init --rootful --cpus 6 --memory 12288 --disk-size 80 "${machine}"
    fi
    if [[ "$(podman machine inspect --format '{{.State}}' "${machine}")" != running ]]; then
      # Podman runs one VM at a time; never stop another machine's containers here.
      other="$(podman machine list --format '{{.Name}} {{.Running}}' | awk -v me="${machine}" '$2 == "true" && $1 != me {print $1}')"
      if [[ -n "${other}" ]]; then
        echo "Podman machine ${other} is running and only one VM can be active: podman machine stop ${other}" >&2
        exit 2
      fi
      echo "Starting the ${machine} Podman machine..."
      podman machine start "${machine}"
    fi
    podman_socket="$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}' "${machine}")"
    export DOCKER_HOST="unix://${podman_socket}"
  fi
fi
echo "Container engine: ${DOCKER_HOST}"

state="${project_root}/target/act"
mkdir -p "${state}/artifacts" "${state}/cache"

cd "${project_root}"
exec act push \
  --workflows .github/workflows/ci.yml \
  --job "${job}" \
  --platform ubuntu-24.04=docker.io/catthehacker/ubuntu:act-24.04 \
  --container-architecture "linux/$(uname -m | sed 's/^x86_64$/amd64/; s/^aarch64$/arm64/')" \
  --pull=false \
  --container-daemon-socket - \
  --artifact-server-path "${state}/artifacts" \
  --cache-server-path "${state}/cache" \
  "$@"
