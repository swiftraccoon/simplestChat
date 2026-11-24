#!/usr/bin/env bash
# Test native DTLS, diagnostics and bandwidth patches in an isolated worker build.
# The check is `simplestchat-ci native-dtls`; this entry point runs it.
set -euo pipefail

if [[ "$#" != 0 ]]; then
  echo 'Usage: build/check-native-dtls.sh' >&2
  exit 2
fi
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_root"
exec cargo run --locked -q -p simplestchat-ci -- native-dtls
