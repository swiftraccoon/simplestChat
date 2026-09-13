#!/usr/bin/env bash
# Finalize only containers identified by the private, root-owned current run.
set -euo pipefail
umask 077
mode="${1:---emergency}"
[[ $# -le 1 && ( "$mode" == --normal || "$mode" == --emergency ) ]]
[[ $(id -u) == 0 && $(uname -s) == Linux ]]
record=/run/simplestchat-bench/current.json
[[ -e /run/simplestchat-bench || -L /run/simplestchat-bench ]] || exit 0
[[ ! -L /run/simplestchat-bench && -d /run/simplestchat-bench && $(stat -c '%u:%a' /run/simplestchat-bench) == 0:700 ]]
if [[ "$mode" == --emergency ]]; then
  [[ ! -L /run/simplestchat-bench/workload.lock ]]
  exec 9<>/run/simplestchat-bench/workload.lock
  [[ -f /run/simplestchat-bench/workload.lock && $(stat -c '%u:%a' /run/simplestchat-bench/workload.lock) == 0:600 ]]
  flock --exclusive --timeout 30 9
fi
[[ -e "$record" ]] || exit 0
[[ ! -L "$record" && $(stat -c '%u:%a' "$record") == 0:600 ]]
jq -e '(.schemaVersion == 1) and (.runId | test("^[a-f0-9]{32}$")) and
  (.endpoint | test("^unix:///[^\u0000-\u001f]+$")) and
  (.serverImage | test("^sha256:[a-f0-9]{64}$")) and
  (.generatorImage | test("^sha256:[a-f0-9]{64}$")) and
  (.output | startswith("/")) and (.finalized | type == "boolean")' "$record" >/dev/null
[[ $(jq -r .finalized "$record") == false ]] || exit 0
run_id=$(jq -r .runId "$record")
output=$(jq -r .output "$record")
endpoint=$(jq -r .endpoint "$record")
[[ ! -L "$output" && -d "$output" && $(realpath -e -- "$output") == "$output" ]]
[[ $(stat -c '%u:%a' "$output") == 0:700 ]]
unset DOCKER_CONTEXT DOCKER_HOST DOCKER_TLS_VERIFY DOCKER_CERT_PATH
docker_owned() { timeout --signal=TERM --kill-after=1s 6s docker --host "$endpoint" "$@"; }
inspect_format='{"id":{{json .Id}},"image":{{json .Image}},"run":{{json (index .Config.Labels "simplestchat.benchmark.run")}},"role":{{json (index .Config.Labels "simplestchat.benchmark.role")}},"network":{{json .HostConfig.NetworkMode}},"state":{{json .State}}}'
cleanup_ok=true
if [[ "$mode" == --emergency ]]; then
  retained_result=''
  replace_result=true
  if [[ -f "$output/result.json" && ! -L "$output/result.json" ]]; then
    replace_result=false
    if retained_result=$(mktemp "$output/result-before-emergency.XXXXXX") &&
      cp -- "$output/result.json" "$retained_result"; then
      replace_result=true
    else
      cleanup_ok=false
      echo 'Could not retain the previous result; leaving it unchanged.' >&2
    fi
  fi
  if [[ "$replace_result" == true ]]; then
    jq -n --arg runId "$run_id" --arg retainedResult "${retained_result##*/}" \
      '{schemaVersion:1,runId:$runId,completed:false,passed:false,error:"supervisor_or_launcher_interrupted",
        retainedResult:(if $retainedResult == "" then null else $retainedResult end)}' >"$output/result.json.tmp"
    mv -- "$output/result.json.tmp" "$output/result.json"
  fi
fi
for role in generator server; do
  cid_path="$output/$role.cid"
  expected_image=$(jq -r --arg role "${role}Image" '.[$role]' "$record")
  cid=''
  if [[ -e "$cid_path" ]]; then
    if [[ -L "$cid_path" || $(stat -c '%u:%a' "$cid_path") != 0:600 ]]; then cleanup_ok=false; continue; fi
    cid=$(<"$cid_path")
    if [[ ! "$cid" =~ ^[a-f0-9]{64}$ ]]; then cleanup_ok=false; continue; fi
  fi
  # Exact predeclared names recover a daemon-side create whose CLI never wrote
  # its CID file. No wildcard/name-prefix deletion or global container lookup.
  name="scbench-${run_id}-${role}"
  if ! found=$(docker_owned container ls --all --quiet --no-trunc --filter "name=^/${name}$"); then cleanup_ok=false; continue; fi
  if [[ -z "$found" ]]; then
    # A timed-out create can finish inside the daemon after its CLI exits.
    # One absent listing is not terminal evidence for an attempted create.
    if [[ ( -n "$cid" || -e "$output/$role.create-attempted" ) && ! -f "$output/$role.removed" ]]; then cleanup_ok=false; fi
    continue
  fi
  if [[ ! "$found" =~ ^[a-f0-9]{64}$ || ( -n "$cid" && "$cid" != "$found" ) ]]; then cleanup_ok=false; continue; fi
  cid="$found"
  if ! docker_owned inspect --format "$inspect_format" "$cid" >"$output/$role.cleanup-before.json"; then cleanup_ok=false; continue; fi
  if ! jq -e --arg id "$cid" --arg image "$expected_image" --arg run "$run_id" --arg role "$role" \
    '.id == $id and .image == $image and .run == $run and .role == $role' "$output/$role.cleanup-before.json" >/dev/null; then cleanup_ok=false; continue; fi
  if [[ $(jq -r .state.Running "$output/$role.cleanup-before.json") == true ]]; then
    stop_seconds=5; [[ "$role" == server ]] && stop_seconds=30
    if ! timeout --signal=TERM --kill-after=1s "$((stop_seconds + 3))s" docker --host "$endpoint" stop --time "$stop_seconds" "$cid" >"$output/$role.stop.log" 2>&1; then
      cleanup_ok=false
      docker_owned kill --signal KILL "$cid" >"$output/$role.forced-kill.log" 2>&1 || true
    fi
  fi
  docker_owned logs --timestamps "$cid" >"$output/$role.log" 2>"$output/$role.logs-error.log" || cleanup_ok=false
  if ! docker_owned inspect --format "$inspect_format" "$cid" >"$output/$role.final-state.json"; then cleanup_ok=false; continue; fi
  if ! jq -e '.state.Running == false and .state.Restarting == false' "$output/$role.final-state.json" >/dev/null; then cleanup_ok=false; continue; fi
  if docker_owned rm "$cid" >"$output/$role.remove.log" 2>&1; then
    printf '%s\n' "$cid" >"$output/$role.removed"
  else cleanup_ok=false; fi
done
jq -n --argjson passed "$cleanup_ok" --arg mode "$mode" '{schemaVersion:1,passed:$passed,mode:$mode}' >"$output/cleanup.json.tmp"
mv -- "$output/cleanup.json.tmp" "$output/cleanup.json"
if [[ "$mode" == --emergency && "$cleanup_ok" == true ]]; then
  jq '.finalized = true' "$record" >"$record.tmp"
  mv -- "$record.tmp" "$record"
fi
[[ "$cleanup_ok" == true ]]
