#!/usr/bin/env bash
# Execute the maintained CI workflow, including its reusable workflows, locally.
set -euo pipefail
umask 077

usage() {
  cat <<'USAGE'
Usage: build/ci-local.sh [all|JOB] [options]

  all                         Run the workflow's required gate and all its needs (default).
  JOB                         Run one job; this is not a complete CI result.
  --base REF                  Changed-source base (default: merge-base with origin/main).
  --matrix KEY:VALUE          Select a matrix entry for a single job only.
  --output NEW_DIRECTORY      Evidence directory (default: a new directory under results/).
  --disposable-engine         Use explicit DOCKER_HOST for an isolated disposable Linux engine.
  --help                      Show this help.

Full CI requires a clean committed checkout, but no push or pull request. It runs
the actual GitHub workflow; hosted signing/publication stays on GitHub. Apple
silicon runs the same suites natively on ARM64; production images remain AMD64.
macOS uses only the owned simplestchat-ci Podman VM. Linux requires
--disposable-engine and DOCKER_HOST; never point it at a shared or production engine.
USAGE
}

die() { printf '%s\n' "$*" >&2; exit 2; }
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${project_root}"
job=all
base_ref=''
output=''
disposable_engine=0
matrix_args=()
if (($#)) && [[ "$1" != -* ]]; then job="$1"; shift; fi
while (($#)); do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --disposable-engine) disposable_engine=1; shift ;;
    --base|--matrix|--output)
      (($# >= 2)) || die "Missing value for $1"
      case "$1" in
        --base) base_ref="$2" ;;
        --matrix) matrix_args+=(--matrix "$2") ;;
        --output) output="$2" ;;
      esac
      shift 2 ;;
    *) die "Unknown option: $1 (see --help)" ;;
  esac
done
[[ "$job" =~ ^[a-z][a-z0-9-]*$ ]] || die 'Invalid job name.'
[[ "$job" != all || ${#matrix_args[@]} == 0 ]] || die 'A full CI run cannot filter out matrix entries.'
[[ "$job" != required && "$job" != release-security ]] || die 'Use all for the aggregate gate; release signing runs only on GitHub.'
for tool in act curl git python3; do command -v "$tool" >/dev/null || die "Missing prerequisite: $tool"; done

revision="$(git rev-parse --verify HEAD)"
if [[ "$job" == all && -n "$(git status --porcelain=v1 --untracked-files=all)" ]]; then
  die 'Full CI requires a clean committed checkout. Commit local changes first; no push is needed.'
fi
if [[ -z "$base_ref" ]]; then
  base_ref="$(git merge-base HEAD refs/remotes/origin/main)" || die 'Pass --base with an existing local commit.'
  if [[ "$base_ref" == "$revision" ]]; then base_ref=HEAD^; fi
fi
base="$(git rev-parse --verify "${base_ref}^{commit}")" || die 'The --base revision must exist locally.'
[[ "$base" != "$revision" ]] || die 'The changed-source base must precede the candidate.'
git merge-base --is-ancestor "$base" "$revision" || die 'The changed-source base must be an ancestor of HEAD.'

state="${project_root}/target/act"
mkdir -p "${state}/actions" "${state}/cache" "${project_root}/results"
mkdir "${state}/run.lock" 2>/dev/null || die "Another local CI invocation owns ${state}/run.lock."
started_machine=0
owned_output=0
machine=simplestchat-ci
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
runner_image=docker.io/catthehacker/ubuntu@sha256:4f2d5083a9d10d018c1c511eb8665cd480553c11975e78fd903a46daa830768b
runner_platform=linux/amd64
cleanup() {
  local status=$?
  trap - EXIT
  ((BASH_SUBSHELL == 0)) || exit "$status"
  if ((started_machine)); then podman machine stop "$machine" || status=1; fi
  rmdir "${state}/run.lock" || status=1
  if ((owned_output)); then
    python3 - "$output" "$revision" "$base" "$job" "$status" "$started_at" "$runner_image" "$runner_platform" <<'PY'
import datetime
import json
import pathlib
import sys

directory, revision, base, job, status, started, image, runner_platform = sys.argv[1:]
report = {
    "schema": 1, "revision": revision, "base": base, "selection": job,
    "runnerPlatform": runner_platform, "runnerImage": image, "exitCode": int(status),
    "productionPlatform": "linux/amd64", "nativeSecurityPlatform": runner_platform,
    "nativeCodeqlPlatform": runner_platform,
    "status": "passed" if status == "0" else "failed",
    "startedAt": started, "finishedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "completeLocalGate": job == "all" and status == "0",
    "hostedOnly": ["CodeQL upload and stored-analysis ingestion", "GitHub OIDC release signing and publication"],
}
pathlib.Path(directory, "summary.json").write_text(json.dumps(report, indent=2) + "\n")
PY
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [[ -z "$output" ]]; then
  output="$(mktemp -d "${project_root}/results/ci-local.XXXXXXXX")"
else
  [[ "$output" == /* ]] || output="${project_root}/${output}"
  [[ ! -e "$output" && ! -L "$output" ]] || die '--output must be a new directory.'
  mkdir "$output"
  output="$(cd "$output" && pwd)"
fi
owned_output=1
printf 'Local CI evidence: %s\n' "$output"

if ((disposable_engine)); then
  [[ "${DOCKER_HOST:-}" == unix:///* ]] || die '--disposable-engine requires an explicit local unix:// DOCKER_HOST.'
else
  [[ "$(uname -s)" == Darwin ]] || die 'Linux: set DOCKER_HOST for an isolated engine and pass --disposable-engine.'
  [[ -z "${DOCKER_HOST:-}" ]] || die 'Explicit DOCKER_HOST requires --disposable-engine; it is never selected implicitly.'
  if [[ "$(uname -m)" == arm64 ]]; then
    runner_image=docker.io/catthehacker/ubuntu@sha256:84e94c96278dd26b8feb71226a521d259f8160cf6b1dd08e51fd42be43a82e56
    runner_platform=linux/arm64
  fi
  command -v podman >/dev/null || die 'Install Podman to use the owned simplestchat-ci VM.'
  if ! podman machine inspect "$machine" >/dev/null 2>&1; then
    podman machine init --rootful --cpus 12 --memory 49152 --disk-size 80 "$machine"
  fi
  if [[ "$(podman machine inspect --format '{{.State}}' "$machine")" != running ]]; then
    other="$(podman machine list --format '{{.Name}} {{.Running}}' | awk -v me="$machine" '$2 == "true" && $1 != me {print $1}')"
    [[ -z "$other" ]] || die "Another Podman VM is running ($other); it was left untouched."
    podman machine start "$machine"
    started_machine=1
  fi
  DOCKER_HOST="unix://$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}' "$machine")"
  export DOCKER_HOST
fi
engine_socket="${DOCKER_HOST#unix://}"
curl --silent --show-error --fail --max-time 10 --unix-socket "$engine_socket" http://localhost/_ping >/dev/null || die 'The owned container engine is not responding.'
curl --silent --show-error --fail --max-time 10 --unix-socket "$engine_socket" \
  'http://localhost/containers/json?all=1' > "$output/engine-before.json"
python3 - "$output/engine-before.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as source:
    if json.load(source):
        raise SystemExit("The disposable engine contains existing containers; they were left untouched.")
PY

# Select the packaged emulator only after refusing existing containers. Rosetta
# lacks Fedora 44's required syscalls; the job-owned Docker daemon stays native.
if ((!disposable_engine)) && [[ "$runner_platform" == linux/arm64 ]]; then
  podman machine ssh "$machine" 'sudo sh -se' <<'SH'
test -x /usr/bin/qemu-x86_64-static
/usr/lib/systemd/systemd-binfmt /usr/lib/binfmt.d/qemu-x86_64-static.conf
if test -e /proc/sys/fs/binfmt_misc/rosetta; then
  printf '0\n' > /proc/sys/fs/binfmt_misc/rosetta
  grep -qx disabled /proc/sys/fs/binfmt_misc/rosetta
fi
grep -qx enabled /proc/sys/fs/binfmt_misc/qemu-x86_64
grep -qx 'interpreter /usr/bin/qemu-x86_64-static' /proc/sys/fs/binfmt_misc/qemu-x86_64
SH
fi

curl --silent --show-error --fail --max-time 10 --unix-socket "$engine_socket" \
  http://localhost/networks > "$output/engine-networks.json"
gateway="$(python3 - "$output/engine-networks.json" <<'PY'
import ipaddress
import json
import sys
with open(sys.argv[1]) as source:
    networks = json.load(source)
addresses = [entry["Gateway"] for network in networks if network.get("Name") == "bridge"
             for entry in network.get("IPAM", {}).get("Config", []) if "Gateway" in entry]
addresses = [address for address in addresses if ipaddress.ip_address(address).version == 4]
if len(addresses) != 1 or not ipaddress.ip_address(addresses[0]).is_private:
    raise SystemExit("The isolated engine must expose one private IPv4 bridge gateway")
print(addresses[0])
PY
)"

run_id="$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
python3 - "$output/event.json" "$revision" "$base" <<'PY'
import json
import pathlib
import sys
path, revision, base = sys.argv[1:]
pathlib.Path(path).write_text(json.dumps({
    "ref": "refs/heads/main", "before": base, "after": revision,
    "head_commit": {"id": revision}, "local_ci": True,
    "repository": {"default_branch": "main", "full_name": "swiftraccoon/simplestChat",
                   "name": "simplestChat", "owner": {"name": "swiftraccoon", "login": "swiftraccoon"}},
}) + "\n")
PY
mkdir "$output/checks"
printf -v evidence_mount '%q' "type=bind,source=${output}/checks,target=/local-ci-evidence"
container_options="--privileged --cgroupns=private --cpus=3 --memory=8g --pids-limit=2048 --add-host=host.docker.internal:${gateway} --mount type=volume,target=/var/lib/local-ci-docker --mount ${evidence_mount}"
# Reuse only the pinned cache for this runner. A fresh checkout still lets
# each job install its authenticated bundle into its disposable workspace.
if [[ -d "$project_root/target/codeql-tools" ]] && python3 - "$project_root" "$runner_platform" <<'PY'
import json
import os
import pathlib
import sys
root = pathlib.Path(sys.argv[1])
bundles = json.loads((root / "security/codeql-toolchain.json").read_text())["bundles"]
platform = "linux-aarch64" if sys.argv[2] == "linux/arm64" else "linux-x86_64"
try:
    pin = bundles[platform]
    directory = root / "target/codeql-tools" / pin["sha256"]
    if (json.loads((directory / "receipt.json").read_text()) != {
            "archiveSha256": pin["sha256"], "archiveBytes": pin["bytes"]}
            or not os.access(directory / "codeql/codeql", os.X_OK)):
        raise SystemExit(1)
except (OSError, ValueError, KeyError):
    raise SystemExit(1)
PY
then
  printf -v codeql_mount '%q' "type=bind,source=${project_root}/target/codeql-tools,target=${project_root}/target/codeql-tools,readonly"
  container_options+=" --mount ${codeql_mount}"
fi
selected_job="$job"
[[ "$job" != all ]] || selected_job=required
set +e
act push --workflows .github/workflows/ci.yml --job "$selected_job" \
  --eventpath "$output/event.json" --defaultbranch main \
  --platform "ubuntu-24.04=${runner_image}" \
  --container-architecture '' \
  --pull=false --rm --network bridge --concurrent-jobs 1 \
  --dryrun=false --list=false --graph=false --validate=false --watch=false --reuse=false \
  --bind=false --no-skip-checkout=false --list-options=false --bug-report=false --man-page=false \
  --container-daemon-socket - --container-options "$container_options" \
  --use-new-action-cache=true --action-cache-path "${state}/actions" --cache-server-path "${state}/cache" \
  --env-file /dev/null --secret-file /dev/null --var-file /dev/null --input-file /dev/null \
  --secret GITHUB_TOKEN= --env LOCAL_CI_DISPOSABLE=1 --env LOCAL_CI_EVIDENCE=/local-ci-evidence \
  --env "LOCAL_CI_RUN_ID=${run_id}" \
  ${matrix_args[@]+"${matrix_args[@]}"} \
  2>&1 | tee "$output/act.log"
statuses=("${PIPESTATUS[@]}")
set -e
if [[ "$job" == all && "${statuses[0]}" == 0 ]]; then
  [[ "$(git rev-parse --verify HEAD)" == "$revision" && -z "$(git status --porcelain=v1 --untracked-files=all)" ]] || die 'The candidate changed during CI; this result does not validate the current source.'
  python3 - "$output/checks" "$revision" "$base" "$run_id" "$project_root" <<'PY'
import hashlib
import json
import pathlib
import sys
directory, revision, base, run_id, root = sys.argv[1:]
evidence = pathlib.Path(directory)
receipt = json.loads((evidence / "required.json").read_text())
identity = {"runId": run_id, "revision": revision, "base": base}
expected_hashes = {name: hashlib.sha256(pathlib.Path(root, ".github/workflows", name).read_bytes()).hexdigest()
                   for name in ("ci.yml", "security.yml", "codeql.yml")}
if (receipt.get("schema") != 1 or receipt.get("status") != "passed"
        or any(receipt.get(key) != value for key, value in identity.items())
        or receipt.get("workflows") != expected_hashes
        or not receipt.get("gates") or not receipt.get("checks")):
    raise SystemExit("The real aggregate gate did not validate this exact local run")
for identifier in receipt["checks"]:
    check = json.loads((evidence / "receipts" / f"{identifier}.json").read_text())
    if check != {"schema": 1, "status": "passed", "check": identifier, **identity}:
        raise SystemExit("A required matrix entry has no matching successful execution receipt")
PY
fi
curl --silent --show-error --fail --max-time 10 --unix-socket "$engine_socket" \
  'http://localhost/containers/json?all=1' > "$output/engine-after.json"
python3 - "$output/engine-after.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as source:
    if json.load(source):
        raise SystemExit("Local CI left containers behind; inspect the owned engine before another run.")
PY
((statuses[0] == 0)) || exit "${statuses[0]}"
((statuses[1] == 0)) || exit "${statuses[1]}"
