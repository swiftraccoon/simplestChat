set -euo pipefail
clients="${CLIENTS}"
workers="${WORKERS}"
if [[ "${clients}" = "30" ]]; then
  scenario=multi-room; ramp=65
  extra=(--subscription-plan ring-v1 --subscription-seed 17)
elif [[ "${clients}" = "100" ]]; then
  scenario=multi-room; ramp=202
  extra=(--subscription-plan ring-v1 --subscription-seed 17)
else
  scenario=conference; ramp=5
  extra=()
fi
"${GITHUB_WORKSPACE}/target/release/load_test" bench local \
  --baseline-root "${RUNNER_TEMP}/baseline" \
  --baseline-bin "${RUNNER_TEMP}/baseline/target/release/simplestChat" \
  --candidate-root "${GITHUB_WORKSPACE}" \
  --candidate-bin "${GITHUB_WORKSPACE}/target/release/simplestChat" \
  --generator "${GITHUB_WORKSPACE}/target/release/load_test" \
  --output "${RUNNER_TEMP}/comparison" \
  --clients "${clients}" --scenarios "${scenario}" --repetitions 3 \
  --ramp-up "${ramp}" --warmup 10 --duration 60 --workers "${workers}" "${extra[@]}"
