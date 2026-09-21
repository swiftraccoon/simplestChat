#!/bin/sh
# Owned local server plus the impaired-network browser scenario.
#
#   build/impaired-check.sh [results-dir]
#
# Environment: SIMPLESTCHAT_BIN (default target/release/simplestChat),
# IMPAIRED_PROFILES, IMPAIR_SCRIPT ("none" for mechanics only), E2E_BROWSER,
# LIBWEBRTC_FIELD_TRIALS, WEBRTC_MIN_OUTGOING_BITRATE and WEBRTC_SERVER_TCP
# (passed to the server; the last one with IMPAIRED_BLOCK_UDP=1 proves ICE-TCP).
# The server is guest-only with ad-hoc rooms on 127.0.0.1:3109 and media port
# 41100; the scenario applies impairment through build/impair.sh (sudo) and
# clears it before exit. The UI must be built and web/e2e installed.
set -eu
cd "$(dirname "$0")/.."
binary="${SIMPLESTCHAT_BIN:-target/release/simplestChat}"
results="${1:-results/impaired-network.$(date -u +%Y%m%dT%H%M%SZ)}"
port=3109
udp_port=41100
mkdir -p "${results}"
test -x "${binary}" || { echo "Server binary ${binary} is missing" >&2; exit 2; }
env -i PATH="${PATH}" HOME="${HOME}" TMPDIR="${TMPDIR:-/tmp}" \
  ${LIBWEBRTC_FIELD_TRIALS:+LIBWEBRTC_FIELD_TRIALS="${LIBWEBRTC_FIELD_TRIALS}"} \
  ${WEBRTC_MIN_OUTGOING_BITRATE:+WEBRTC_MIN_OUTGOING_BITRATE="${WEBRTC_MIN_OUTGOING_BITRATE}"} \
  ${WEBRTC_SERVER_TCP:+WEBRTC_SERVER_TCP="${WEBRTC_SERVER_TCP}"} \
  BIND_ADDR=127.0.0.1 PORT="${port}" ANNOUNCE_IP=127.0.0.1 MEDIA_WORKERS=1 \
  WEBRTC_SERVER_PORT_BASE="${udp_port}" ALLOW_AD_HOC_ROOMS=true \
  ALLOWED_ORIGINS="http://127.0.0.1:${port}" REGISTRATION_ENABLED=false \
  RUST_LOG=simplestChat=info,mediasoup=warn "${binary}" > "${results}/server.log" 2>&1 &
server=$!
cleanup() {
  kill -TERM "${server}" 2>/dev/null || true
  wait "${server}" 2>/dev/null || true
  IMPAIR_UDP_PORT="${udp_port}" sh build/impair.sh clear >/dev/null 2>&1 || true
  IMPAIR_UDP_PORT="${udp_port}" sh build/impair.sh unblock-udp >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
for _ in $(seq 1 100); do
  if curl -fsS "http://127.0.0.1:${port}/ready" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
curl -fsS "http://127.0.0.1:${port}/ready" >/dev/null || { echo "Owned server did not become ready" >&2; exit 1; }
BASE_URL="http://127.0.0.1:${port}" IMPAIR_UDP_PORT="${udp_port}" IMPAIR_WORKERS=1 \
  E2E_ARTIFACTS="${results}" node web/e2e/impaired-network.cjs
echo "Results: ${results}"
