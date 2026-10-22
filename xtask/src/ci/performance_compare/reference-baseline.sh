set -euo pipefail
baseline="$(git rev-parse --verify "${BASELINE_REF}^{commit}")"
echo "baseline commit ${baseline}"
git worktree add --detach "${RUNNER_TEMP}/baseline" "${baseline}"
find "${RUNNER_TEMP}/baseline/vendor" -exec touch -h -t 200001010000 {} +
(
  cd "${RUNNER_TEMP}/baseline"
  CARGO_TARGET_DIR="${GITHUB_WORKSPACE}/target" \
    cargo build --locked --release --bin simplestChat
)
install -D "${GITHUB_WORKSPACE}/target/release/simplestChat" \
  "${RUNNER_TEMP}/baseline/target/release/simplestChat"
rm -f "${GITHUB_WORKSPACE}/target/release/simplestChat"
