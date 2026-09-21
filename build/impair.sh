#!/bin/sh
# Impair the UDP media path of an owned local test server on loopback.
#
#   build/impair.sh apply        # from IMPAIR_* environment
#   build/impair.sh clear
#   build/impair.sh block-udp    # drop every UDP packet to or from the media
#   build/impair.sh unblock-udp  # port(s), so clients must connect over TCP
#
# Environment:
#   IMPAIR_UDP_PORT   server media port (default 41100); with IMPAIR_WORKERS>1
#                     the consecutive ports up to a power of two are covered
#   IMPAIR_WORKERS    media workers sharing consecutive ports (default 1)
#   IMPAIR_LOSS       packet loss in percent (default 0)
#   IMPAIR_DELAY_MS   one-way delay in milliseconds (default 0)
#   IMPAIR_JITTER_MS  delay variation in milliseconds; Linux only (default 0)
#   IMPAIR_RATE_KBIT  bandwidth cap in kbit/s, 0 for none (default 0)
#   IMPAIR_DIRECTION  downlink (packets from the server port) or both (default downlink)
#
# Linux uses tc/netem on lo with a UDP filter; macOS uses pf dummynet through
# an anchor under com.apple/ that the stock pf.conf already evaluates, enabling
# pf with a reference token that clear releases. Both need sudo and touch only
# the named anchor/qdisc; nothing else on the host changes. Never run against
# an interface carrying real users.
set -eu

port="${IMPAIR_UDP_PORT:-41100}"
workers="${IMPAIR_WORKERS:-1}"
loss="${IMPAIR_LOSS:-0}"
delay_ms="${IMPAIR_DELAY_MS:-0}"
jitter_ms="${IMPAIR_JITTER_MS:-0}"
rate_kbit="${IMPAIR_RATE_KBIT:-0}"
direction="${IMPAIR_DIRECTION:-downlink}"
state_dir="${IMPAIR_STATE_DIR:-${TMPDIR:-/tmp}}"
token_file="${state_dir}/simplestchat-impair.pf-token"
block_token_file="${state_dir}/simplestchat-block.pf-token"
anchor="com.apple/simplestchat-impair"
block_anchor="com.apple/simplestchat-block"
pipe=41
platform="$(uname -s)"

case "${direction}" in downlink|both) ;; *) echo "IMPAIR_DIRECTION must be downlink or both" >&2; exit 2 ;; esac
case "${workers}" in 1) span=1 ;; 2) span=2 ;; 3|4) span=4 ;; *) echo "IMPAIR_WORKERS must be 1-4" >&2; exit 2 ;; esac
if [ $((port % span)) -ne 0 ]; then echo "IMPAIR_UDP_PORT must be a multiple of ${span}" >&2; exit 2; fi
last_port=$((port + span - 1))

linux_apply() {
  params="limit 200000"
  [ "${loss}" != "0" ] && params="${params} loss ${loss}%"
  if [ "${delay_ms}" != "0" ]; then
    params="${params} delay ${delay_ms}ms"
    [ "${jitter_ms}" != "0" ] && params="${params} ${jitter_ms}ms"
  fi
  [ "${rate_kbit}" != "0" ] && params="${params} rate ${rate_kbit}kbit"
  mask="$(printf '0x%x' $((0x10000 - span)))"
  sudo tc qdisc add dev lo root handle 1: prio bands 3 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  # shellcheck disable=SC2086
  sudo tc qdisc add dev lo parent 1:3 handle 30: netem ${params}
  sudo tc filter add dev lo parent 1: protocol ip prio 1 u32 match ip protocol 17 0xff match ip sport "${port}" "${mask}" flowid 1:3
  if [ "${direction}" = "both" ]; then
    sudo tc filter add dev lo parent 1: protocol ip prio 2 u32 match ip protocol 17 0xff match ip dport "${port}" "${mask}" flowid 1:3
  fi
  sudo tc qdisc show dev lo
}

linux_clear() {
  sudo tc qdisc del dev lo root 2>/dev/null || true
}

darwin_apply() {
  config="delay ${delay_ms}"
  [ "${loss}" != "0" ] && config="${config} plr $(awk "BEGIN { printf \"%.4f\", ${loss} / 100 }")"
  [ "${rate_kbit}" != "0" ] && config="${config} bw ${rate_kbit}Kbit/s"
  # shellcheck disable=SC2086
  sudo dnctl pipe "${pipe}" config ${config}
  rules="dummynet in proto udp from any port ${port}:${last_port} to any pipe ${pipe}
"
  if [ "${direction}" = "both" ]; then
    rules="${rules}dummynet in proto udp from any to any port ${port}:${last_port} pipe ${pipe}
"
  fi
  printf '%s' "${rules}" | sudo pfctl -q -a "${anchor}" -f -
  if [ ! -f "${token_file}" ]; then
    sudo pfctl -E 2>&1 | sed -n 's/.*Token : \([0-9][0-9]*\).*/\1/p' > "${token_file}"
    [ -s "${token_file}" ] || { rm -f "${token_file}"; echo "pfctl -E returned no token" >&2; exit 1; }
  fi
  sudo dnctl list
  sudo pfctl -q -a "${anchor}" -s rules
}

darwin_clear() {
  sudo pfctl -q -a "${anchor}" -F all 2>/dev/null || true
  sudo dnctl pipe "${pipe}" delete 2>/dev/null || true
  if [ -f "${token_file}" ]; then
    sudo pfctl -X "$(cat "${token_file}")" 2>/dev/null || true
    rm -f "${token_file}"
  fi
}

# The block lives apart from the netem/dummynet state so apply and clear can
# run for the phases while it stays in force.
linux_block_udp() {
  linux_unblock_udp
  sudo iptables -I INPUT -i lo -p udp --dport "${port}:${last_port}" -j DROP
  sudo iptables -I INPUT -i lo -p udp --sport "${port}:${last_port}" -j DROP
  sudo iptables -S INPUT | grep -- "--dport ${port}:${last_port}\|--sport ${port}:${last_port}"
}

linux_unblock_udp() {
  sudo iptables -D INPUT -i lo -p udp --dport "${port}:${last_port}" -j DROP 2>/dev/null || true
  sudo iptables -D INPUT -i lo -p udp --sport "${port}:${last_port}" -j DROP 2>/dev/null || true
}

darwin_block_udp() {
  rules="block drop quick on lo0 proto udp from any to any port ${port}:${last_port}
block drop quick on lo0 proto udp from any port ${port}:${last_port} to any
"
  printf '%s' "${rules}" | sudo pfctl -q -a "${block_anchor}" -f -
  if [ ! -f "${block_token_file}" ]; then
    sudo pfctl -E 2>&1 | sed -n 's/.*Token : \([0-9][0-9]*\).*/\1/p' > "${block_token_file}"
    [ -s "${block_token_file}" ] || { rm -f "${block_token_file}"; echo "pfctl -E returned no token" >&2; exit 1; }
  fi
  sudo pfctl -q -a "${block_anchor}" -s rules
}

darwin_unblock_udp() {
  sudo pfctl -q -a "${block_anchor}" -F all 2>/dev/null || true
  if [ -f "${block_token_file}" ]; then
    sudo pfctl -X "$(cat "${block_token_file}")" 2>/dev/null || true
    rm -f "${block_token_file}"
  fi
}

case "$1" in
  apply)
    # Replacing a profile mid-run must not fail on the previous qdisc or pipe.
    case "${platform}" in
      Linux) linux_clear; linux_apply ;;
      Darwin) darwin_clear; darwin_apply ;;
      *) echo "Unsupported platform ${platform}" >&2; exit 2 ;;
    esac
    echo "impairment applied: port ${port}-${last_port} ${direction} loss=${loss}% delay=${delay_ms}ms jitter=${jitter_ms}ms rate=${rate_kbit}kbit"
    ;;
  clear)
    case "${platform}" in
      Linux) linux_clear ;;
      Darwin) darwin_clear ;;
      *) echo "Unsupported platform ${platform}" >&2; exit 2 ;;
    esac
    echo "impairment cleared"
    ;;
  block-udp)
    case "${platform}" in
      Linux) linux_block_udp ;;
      Darwin) darwin_block_udp ;;
      *) echo "Unsupported platform ${platform}" >&2; exit 2 ;;
    esac
    echo "udp blocked: port ${port}-${last_port} both directions"
    ;;
  unblock-udp)
    case "${platform}" in
      Linux) linux_unblock_udp ;;
      Darwin) darwin_unblock_udp ;;
      *) echo "Unsupported platform ${platform}" >&2; exit 2 ;;
    esac
    echo "udp unblocked"
    ;;
  *) echo "Usage: build/impair.sh apply|clear|block-udp|unblock-udp" >&2; exit 2 ;;
esac
