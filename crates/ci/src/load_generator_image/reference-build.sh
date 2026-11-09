set -euo pipefail
test "$(git rev-parse HEAD)" = "${GITHUB_SHA}"
docker buildx rm --force simplestchat-loadtest 2>/dev/null || true
docker buildx create --name simplestchat-loadtest --driver docker-container --use
cache_dir="${RUNNER_TEMP}/buildx-cache"
cache_from=()
if [[ -f "${cache_dir}/index.json" ]]; then
  cache_from=(--cache-from "type=local,src=${cache_dir}")
fi
timeout --signal=TERM --kill-after=2m 40m \
  docker buildx build --pull --load --target loadtest --platform linux/amd64 \
    --provenance=false --sbom=false \
    "${cache_from[@]}" \
    --build-arg "SOURCE_REVISION=${GITHUB_SHA}" \
    --label "org.opencontainers.image.revision=${GITHUB_SHA}" \
    --tag simplestchat-loadtest:ci .
docker buildx rm --force simplestchat-loadtest
mkdir -p "${RUNNER_TEMP}/loadtest-image"
docker save simplestchat-loadtest:ci \
  | zstd -T0 -3 -o "${RUNNER_TEMP}/loadtest-image/simplestchat-loadtest-${GITHUB_SHA}.tar.zst"
docker image inspect --format '{{.Id}}' simplestchat-loadtest:ci \
  > "${RUNNER_TEMP}/loadtest-image/image-id.txt"
ls -la "${RUNNER_TEMP}/loadtest-image"
