#!/bin/sh
# Owned local server plus the synthetic generator under an impaired media path.
#
#   build/impaired-generator.sh [results-dir]
#
# Environment: SIMPLESTCHAT_BIN (default target/release/simplestChat),
# LOAD_TEST_BIN (default target/release/load_test), IMPAIR_* for the profile
# (default five percent loss with 50 ms of delay and 10 ms of jitter in both
# directions), CLIENTS (30), DURATION (60).
#
# The generator's exact-delivery gates describe a lossless loopback; under
# impairment they are evidence, not a verdict, so this run retains the summary
# and per-client reports and succeeds when the generator completes its run and
# the server exits cleanly. Delivery, keyframe requests, bandwidth estimates
# and connection failures are read from the retained reports.
set -eu
cd "$(dirname "$0")/.."
binary="${SIMPLESTCHAT_BIN:-target/release/simplestChat}"
generator="${LOAD_TEST_BIN:-target/release/load_test}"
results="${1:-results/impaired-generator.$(date -u +%Y%m%dT%H%M%SZ)}"
clients="${CLIENTS:-30}"
duration="${DURATION:-60}"
port=3129
udp_port=41100
mkdir -p "${results}"
test -x "${binary}" || { echo "Server binary ${binary} is missing" >&2; exit 2; }
test -x "${generator}" || { echo "Generator ${generator} is missing" >&2; exit 2; }
export IMPAIR_UDP_PORT="${udp_port}" IMPAIR_WORKERS=1
export IMPAIR_DIRECTION="${IMPAIR_DIRECTION:-both}" IMPAIR_LOSS="${IMPAIR_LOSS:-5}"
export IMPAIR_DELAY_MS="${IMPAIR_DELAY_MS:-50}" IMPAIR_JITTER_MS="${IMPAIR_JITTER_MS:-10}"
env -i PATH="${PATH}" HOME="${HOME}" TMPDIR="${TMPDIR:-/tmp}" \
  ${LIBWEBRTC_FIELD_TRIALS:+LIBWEBRTC_FIELD_TRIALS="${LIBWEBRTC_FIELD_TRIALS}"} \
  BIND_ADDR=127.0.0.1 PORT="${port}" ANNOUNCE_IP=127.0.0.1 MEDIA_WORKERS=1 \
  WEBRTC_SERVER_PORT_BASE="${udp_port}" ALLOW_AD_HOC_ROOMS=true \
  ALLOWED_ORIGINS="http://127.0.0.1:${port}" REGISTRATION_ENABLED=false \
  MAX_CONNECTIONS_PER_IP=128 WS_HANDSHAKES_PER_MINUTE=600 \
  RUST_LOG=simplestChat=info,mediasoup=warn "${binary}" > "${results}/server.log" 2>&1 &
server=$!
status=0
cleanup() {
  sh build/impair.sh clear >/dev/null 2>&1 || true
  if kill -0 "${server}" 2>/dev/null; then
    kill -TERM "${server}" 2>/dev/null || true
    wait "${server}" 2>/dev/null || status=$?
  fi
}
trap cleanup EXIT INT TERM
for _ in $(seq 1 100); do
  if curl -fsS "http://127.0.0.1:${port}/ready" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
curl -fsS "http://127.0.0.1:${port}/ready" >/dev/null || { echo "Owned server did not become ready" >&2; exit 1; }
sh build/impair.sh apply | tee "${results}/impairment.txt"
generator_status=0
"${generator}" --server "ws://127.0.0.1:${port}/ws" --clients "${clients}" --rooms 4 \
  --room "impaired-$(date -u +%H%M%S)" --duration "${duration}" --ramp-up 65 --warmup 10 \
  --mode conference --quality 480p --fps 30 --max-audio 4 --max-video 4 \
  --subscription-plan ring-v1 --subscription-seed 17 --output-dir "${results}" \
  --run-label impaired > "${results}/generator.log" 2>&1 || generator_status=$?
if command -v tc >/dev/null 2>&1; then tc -s qdisc show dev lo > "${results}/netem-stats.txt" 2>/dev/null || true; fi
sh build/impair.sh clear
kill -TERM "${server}" 2>/dev/null || true
wait "${server}" || status=$?
echo "generator exit ${generator_status}; server exit ${status}" | tee "${results}/exit.txt"
test -s "${results}/load_test_summary.json" || { echo "Generator produced no summary" >&2; exit 1; }
test "${status}" -eq 0 || { echo "Server did not exit cleanly" >&2; exit 1; }
node -e '
const summary = require(require("node:path").resolve(process.argv[1]));
const fields = ["successfulConnections", "failedConnections", "validatedConsumers", "failedConsumers", "keyframesRequested", "bandwidthEstimates", "totalPacketsReceived", "totalErrors"];
console.log(JSON.stringify(Object.fromEntries(fields.map((key) => [key, summary[key]]))));
' "${results}/load_test_summary.json"
echo "Results: ${results}"
