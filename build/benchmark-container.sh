#!/usr/bin/env bash
# Bounded synthetic forwarding on two owned, loopback-only Docker containers.
set -euo pipefail
umask 077
server_image='' generator_image='' output='' workers=1 clients=10 rooms=1 duration=60 ramp_up=5 warmup=10
declare -A seen=()
while (($#)); do
  [[ $# -ge 2 && -z ${seen[$1]:-} ]] || { echo 'Missing or duplicate option' >&2; exit 2; }
  seen[$1]=1
  case "$1" in
    --server-image) server_image=$2 ;; --generator-image) generator_image=$2 ;; --output) output=$2 ;;
    --workers) workers=$2 ;; --clients) clients=$2 ;; --rooms) rooms=$2 ;; --duration) duration=$2 ;;
    --ramp-up) ramp_up=$2 ;; --warmup) warmup=$2 ;; *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
  shift 2
done
[[ $(id -u) == 0 && $(uname -s) == Linux ]]
[[ "$server_image" =~ ^sha256:[a-f0-9]{64}$ && "$generator_image" =~ ^sha256:[a-f0-9]{64}$ ]]
bounded() { [[ "$1" =~ ^[1-9][0-9]{0,2}$ ]] && ((10#$1 >= $2 && 10#$1 <= $3)); }
bounded "$workers" 1 4 && bounded "$clients" 2 30 && bounded "$rooms" 1 4 || exit 2
bounded "$duration" 3 180 && bounded "$ramp_up" 1 120 && bounded "$warmup" 10 60 || exit 2
((clients <= rooms * 10 && rooms <= clients))
for required in docker jq curl nsenter flock timeout realpath od awk sha256sum; do command -v "$required" >/dev/null; done
[[ "$output" == /* && ! -e "$output" && ! -L "$output" ]]
[[ "$output" != *,* && ! "$output" =~ [[:cntrl:]] ]]
output="$(realpath -e -- "$(dirname -- "$output")")/$(basename -- "$output")"
if [[ -e /run/simplestchat-bench || -L /run/simplestchat-bench ]]; then
  [[ ! -L /run/simplestchat-bench && -d /run/simplestchat-bench && $(stat -c '%u:%a' /run/simplestchat-bench) == 0:700 ]]
else mkdir -m 700 /run/simplestchat-bench; fi
[[ ! -L /run/simplestchat-bench/workload.lock ]]
exec 9<>/run/simplestchat-bench/workload.lock
[[ -f /run/simplestchat-bench/workload.lock && $(stat -c '%u:%a' /run/simplestchat-bench/workload.lock) == 0:600 ]]
flock --exclusive --nonblock 9
endpoint="${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}')}"
[[ "$endpoint" =~ ^unix:///[^[:cntrl:]]+$ ]]
unset DOCKER_CONTEXT DOCKER_HOST DOCKER_TLS_VERIFY DOCKER_CERT_PATH
docker_owned() { timeout --signal=TERM --kill-after=1s 8s docker --host "$endpoint" "$@"; }
require_public_chat_stopped() {
  local running_public
  running_public=$(docker_owned ps --quiet --filter 'label=com.docker.compose.project=simplestchat-public') || {
    echo 'Cannot verify whether public chat is running; refusing the private benchmark.' >&2; return 1;
  }
  [[ -z "$running_public" ]] || {
    echo 'Public chat is running; stop it explicitly before starting a private benchmark.' >&2; return 1;
  }
}
require_public_chat_stopped
for image in "$server_image" "$generator_image"; do
  [[ $(docker_owned image inspect --format '{{.Id}}' "$image") == "$image" ]]
  [[ $(docker_owned image inspect --format '{{.Config.User}}' "$image") == 10001:10001 ]]
  docker_owned image inspect --format '{{json .Config.Env}}' "$image" |
    jq -e 'all(.[]; test("^(DATABASE_URL|JWT_SECRET|TURN_URLS|TURN_SECRET|DIAGNOSTICS_PATH|MEDIA_DIAGNOSTICS_ENABLED)=") | not)' >/dev/null
done
docker_owned image inspect --format '{{json .Config}}' "$server_image" | jq -e '.Cmd == ["/app/simplestChat"] and ((.Entrypoint // []) == [])' >/dev/null
docker_owned image inspect --format '{{json .Config.Entrypoint}}' "$generator_image" | jq -e '. == ["/app/load_test"]' >/dev/null
mkdir -m 700 -- "$output"
record=/run/simplestchat-bench/current.json
if [[ -e "$record" ]]; then
  [[ ! -L "$record" && $(stat -c '%u:%a' "$record") == 0:600 ]]
  jq -e '.finalized == true' "$record" >/dev/null
fi
run_id=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
jq -n --arg runId "$run_id" --arg output "$output" --arg endpoint "$endpoint" \
  --arg serverImage "$server_image" --arg generatorImage "$generator_image" \
  '{schemaVersion:1,runId:$runId,output:$output,endpoint:$endpoint,serverImage:$serverImage,generatorImage:$generatorImage,finalized:false}' >"$record.tmp"
mv -- "$record.tmp" "$record"
cp -- "$record" "$output/run.json"
server_id='' generator_id='' sampler_pid='' workload_ok=false phase=setup
cleanup_helper="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/cleanup-container-benchmark.sh"
finish() {
  status=$?
  trap - EXIT INT TERM
  if [[ -n "$sampler_pid" ]]; then kill -TERM "$sampler_pid" 2>/dev/null || true; wait "$sampler_pid" 2>/dev/null || true; fi
  if declare -F host_snapshot >/dev/null && [[ ! -d "$output/host-after" ]]; then host_snapshot after || true; fi
  cleanup_ok=false
  if "$cleanup_helper" --normal; then cleanup_ok=true; fi
  passed=false
  if [[ "$status" == 0 && "$workload_ok" == true && "$cleanup_ok" == true ]]; then passed=true; fi
  jq -n --arg runId "$run_id" --arg phase "$phase" --argjson passed "$passed" --argjson status "$status" --argjson cleanupPassed "$cleanup_ok" --argjson workloadPassed "$workload_ok" \
    '{schemaVersion:1,runId:$runId,completed:true,passed:$passed,phase:$phase,launcherExit:$status,cleanupPassed:$cleanupPassed,workloadPassed:$workloadPassed}' >"$output/result.json.tmp"
  mv -- "$output/result.json.tmp" "$output/result.json"
  if [[ "$cleanup_ok" == true ]]; then jq '.finalized = true' "$record" >"$record.tmp"; mv -- "$record.tmp" "$record"; fi
  echo "Benchmark evidence: $output (passed=$passed)"
  [[ "$passed" == true ]] && exit 0
  exit 1
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
jq -n '{schemaVersion:1,completed:false,passed:false}' >"$output/result.json"
sha256sum "${BASH_SOURCE[0]}" "$cleanup_helper" >"$output/harness-inputs.sha256"
mkdir -m 700 -- "$output/workload"
chown 10001:10001 -- "$output/workload"
jq -n --argjson clients "$clients" --argjson rooms "$rooms" --argjson workers "$workers" \
  --argjson duration "$duration" --argjson rampUp "$ramp_up" --argjson warmup "$warmup" \
  '{clients:$clients,rooms:$rooms,workers:$workers,duration:$duration,rampUp:$rampUp,warmup:$warmup,
    limitations:["Co-located synthetic RTP, not browser quality or production capacity.","Docker stats memory is container accounting, not process RSS; samples are descriptive.","Logs are rotation-bounded to three 10 MiB files per container."]}' >"$output/configuration.json"
host_snapshot() {
  local phase=$1
  mkdir -- "$output/host-$phase"
  for source in stat loadavg meminfo pressure/cpu pressure/io pressure/memory; do
    cp -- "/proc/$source" "$output/host-$phase/${source//\//-}"
  done
  uname -a >"$output/host-$phase/kernel.txt"
}
host_snapshot before
METRICS_TOKEN=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
export METRICS_TOKEN
protections=(--pull never --read-only --user 10001:10001 --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs '/tmp:rw,nosuid,nodev,noexec,size=64m' --memory 4g --memory-swap 4g --cpus 4 --pids-limit 1024 \
  --ulimit nofile=65536:65536 --ulimit core=0:0 --log-driver local --log-opt max-size=10m --log-opt max-file=3)
phase=server_startup
printf 'attempted\n' >"$output/server.create-attempted"
docker_owned create --name "scbench-$run_id-server" --cidfile "$output/server.cid" \
  --label "simplestchat.benchmark.run=$run_id" --label simplestchat.benchmark.role=server \
  "${protections[@]}" --network none --env METRICS_TOKEN \
  --env REGISTRATION_ENABLED=false --env ALLOW_AD_HOC_ROOMS=true --env RUN_MIGRATIONS=false \
  --env BIND_ADDR=127.0.0.1 --env PORT=3000 --env ANNOUNCE_IP=127.0.0.1 \
  --env ALLOWED_ORIGINS=http://127.0.0.1:3000 --env "MEDIA_WORKERS=$workers" \
  --env WEBRTC_SERVER_PORT_BASE=40000 --env RUST_LOG=error "$server_image" >"$output/server.create.log"
server_id=$(<"$output/server.cid")
docker_owned start "$server_id" >"$output/server.start.log"
inspect_format='{"id":{{json .Id}},"image":{{json .Image}},"run":{{json (index .Config.Labels "simplestchat.benchmark.run")}},"network":{{json .HostConfig.NetworkMode}},"state":{{json .State}}}'
owned_pid() {
  docker_owned inspect --format "$inspect_format" "$server_id" >"$output/server.live.json"
  jq -er --arg id "$server_id" --arg image "$server_image" --arg run "$run_id" \
    'select(.id == $id and .image == $image and .run == $run and .network == "none" and .state.Running == true) | .state.Pid | select(type == "number" and . > 1)' "$output/server.live.json"
}
request() {
  local path=$1 target=$2 pid net_fd status=0
  pid=$(owned_pid) || return 1
  # Hold the namespace open and recheck ownership, avoiding a recycled PID.
  exec {net_fd}<"/proc/$pid/ns/net" || return 1
  if [[ $(owned_pid) != "$pid" ]]; then exec {net_fd}<&-; return 1; fi
  printf 'header = "Authorization: Bearer %s"\n' "$METRICS_TOKEN" |
    timeout --signal=TERM --kill-after=1s 4s nsenter --net="/proc/self/fd/$net_fd" -- \
      curl --disable --config - --noproxy '*' --proto '=http' --fail --silent --show-error \
      --connect-timeout 1 --max-time 2 --max-filesize 65536 "http://127.0.0.1:3000$path" >"$target" || status=$?
  exec {net_fd}<&-
  return "$status"
}
deadline=$((SECONDS + 60))
until request /ready "$output/readiness.json" 2>"$output/readiness-error.log"; do
  ((SECONDS < deadline)); sleep 0.5
done
jq -e '.status == "ready"' "$output/readiness.json" >/dev/null
request /metrics "$output/metrics-before.txt"
phase=generator
printf 'attempted\n' >"$output/generator.create-attempted"
docker_owned create --name "scbench-$run_id-generator" --cidfile "$output/generator.cid" \
  --label "simplestchat.benchmark.run=$run_id" --label simplestchat.benchmark.role=generator \
  "${protections[@]}" --network "container:$server_id" --env RUST_LOG=error \
  --mount "type=bind,src=$output/workload,dst=/results" "$generator_image" \
  --server ws://127.0.0.1:3000/ws --clients "$clients" --rooms "$rooms" --room "benchmark-$run_id" \
  --duration "$duration" --ramp-up "$ramp_up" --warmup "$warmup" --quality 480p --fps 30 \
  --max-audio 4 --max-video 4 --output-dir /results --run-label "$run_id" \
  --server-revision "$server_image" --generator-revision "$generator_image" >"$output/generator.create.log"
generator_id=$(<"$output/generator.cid")
docker_owned start "$generator_id" >"$output/generator.start.log"
declare -A cgroups=()
for role in server generator; do
  cid=$(<"$output/$role.cid")
  pid=$(docker_owned inspect --format '{{.State.Pid}}' "$cid")
  [[ "$pid" =~ ^[1-9][0-9]*$ && "$pid" -gt 1 ]]
  group=$(awk -F: '$1 == "0" && $2 == "" { print $3 }' "/proc/$pid/cgroup")
  [[ "$group" == /* && "$group" != *..* ]]
  cgroups[$role]=$(realpath -e -- "/sys/fs/cgroup$group")
  [[ "${cgroups[$role]}" == /sys/fs/cgroup/* ]]
done
sample_resources() {
  trap - EXIT
  trap 'exit 0' TERM
  while true; do
    printf '%s\n' "$(date --utc +%FT%T.%NZ)" >>"$output/sample-times.txt"
    for role in server generator; do
      group="${cgroups[$role]}"
      # Cgroups disappear after a normal container exit; retain boundary absence.
      if [[ -r "$group/cpu.stat" && -r "$group/memory.current" && -r "$group/memory.events" ]]; then
        jq -nc --arg at "$(date --utc +%FT%T.%NZ)" --arg role "$role" --rawfile uptime /proc/uptime \
          --rawfile cpu "$group/cpu.stat" --rawfile memory "$group/memory.current" --rawfile events "$group/memory.events" \
          '{at:$at,role:$role,uptime:$uptime,cpu:$cpu,memoryCurrent:$memory,memoryEvents:$events}' >>"$output/cgroup-samples.jsonl" || return 1
      fi
    done
    docker_owned stats --no-stream --format '{{json .}}' "$server_id" "$generator_id" >>"$output/docker-stats.jsonl" 2>>"$output/sampling-errors.log" || { echo docker_stats_failed >>"$output/sampling-errors.log"; return 1; }
    sleep 1
  done
}
(sample_resources || echo sampling_failed >>"$output/sampling-errors.log") & sampler_pid=$!
timeout --signal=TERM --kill-after=2s "$((ramp_up + warmup + duration + 60))s" \
  docker --host "$endpoint" wait "$generator_id" >"$output/generator.wait.txt"
[[ $(<"$output/generator.wait.txt") == 0 ]]
kill -TERM "$sampler_pid" 2>/dev/null || true
wait "$sampler_pid" 2>/dev/null || true
sampler_pid=''
[[ ! -s "$output/sampling-errors.log" ]]
summary="$output/workload/load_test_summary.json"
phase=summary_validation
[[ ! -e "$output/workload/load_test_timeout.json" && -f "$summary" && ! -L "$summary" ]]
jq -e --argjson clients "$clients" --argjson duration "$duration" --argjson rooms "$rooms" \
  --argjson rampUp "$ramp_up" --argjson warmup "$warmup" --arg run "$run_id" --arg server "$server_image" --arg generator "$generator_image" \
  '.schemaVersion == 2 and .run.completed == true and .run.passed == true and
   .totalErrors == 0 and .failedConnections == 0 and .failedConsumers == 0 and
   .run.configuration.numClients == $clients and .run.configuration.durationSecs == $duration and
   .run.configuration.numRooms == $rooms and .run.configuration.rampUpSecs == $rampUp and .run.configuration.warmupSecs == $warmup and
   .run.configuration.runLabel == $run and .run.provenance.serverRevision == $server and .run.provenance.generatorRevision == $generator and
   .attemptCoverage.version == 1 and .attemptCoverage.scope == "stable-publishers" and .attemptCoverage.available == true and
   .attemptCoverage.attempts == $clients and .attemptCoverage.passedAttempts == $clients and .attemptCoverage.failedAttempts == 0 and
   .attemptCoverage.missingCoverageAttempts == 0 and .attemptCoverage.skippedShortTailAttempts == 0 and
   .attemptCoverage.requestedChurners == 0 and .attemptCoverage.validatedChurners == 0 and
   .validatedConsumers > 0 and .skippedShortLivedConsumers == 0 and
   .validatedConsumers == ([range(0; $rooms) as $r | (($clients / $rooms | floor) + (if $r < ($clients % $rooms) then 1 else 0 end)) as $n |
     $n * ([$n - 1, 4] | min) * 2] | add)' "$summary" >"$output/summary-validation.txt"
zero_counts() {
  awk '
    /^simplestchat_(rooms|participants|connections)_active / { if (NF != 2 || $2 !~ /^[0-9]+$/) exit 1; n[$1]++; v[$1]=$2 }
    /^simplestchat_participants_snapshot_complete / { if (NF != 2 || $2 != 1) exit 1; n[$1]++ }
    END { for (k in n) if (n[k] != 1) exit 1;
      if (n["simplestchat_rooms_active"] != 1 || v["simplestchat_rooms_active"] != 0 ||
          n["simplestchat_participants_active"] != 1 || v["simplestchat_participants_active"] != 0 ||
          n["simplestchat_connections_active"] != 1 || v["simplestchat_connections_active"] != 0 ||
          n["simplestchat_participants_snapshot_complete"] != 1) exit 1 }' "$1"
}
phase=room_cleanup
deadline=$((SECONDS + 40))
until request /metrics "$output/metrics-cleanup.txt" && zero_counts "$output/metrics-cleanup.txt"; do
  ((SECONDS < deadline)); sleep 0.5
done
host_snapshot after
phase=server_shutdown
timeout --signal=TERM --kill-after=1s 33s docker --host "$endpoint" stop --time 30 "$server_id" >"$output/server.stop.log"
for role in server generator; do
  cid=$(<"$output/$role.cid")
  docker_owned inspect --format '{{json .State}}' "$cid" >"$output/$role.exit.json"
  jq -e '.Running == false and .ExitCode == 0 and .OOMKilled == false and .Error == "" and .Restarting == false' "$output/$role.exit.json" >/dev/null
done
workload_ok=true
phase=complete
