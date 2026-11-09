//! Exact logical shell bodies retained from CI source70; no Git is needed at test runtime.
pub const SOURCE_COMMIT: &str = "166af83deaa7c073e134e19fc30ae365ce0c4af3";
pub const WORKFLOW_SHA256: &str =
    "6018b2aa0068edce9aa6c31eff5c4729ae62943e42ef01c25880f31682f51b7e";
pub const PROMTOOL_SHA256: &str =
    "c28595fd420ad6af9b8d030ee4582e3526d100ac1328a0cf94b0abb12f43e5e6";
pub const PROMTOOL: &str = r###"docker run --rm --network=none --read-only --cap-drop=ALL \
  --security-opt=no-new-privileges:true --memory=128m --cpus=0.25 --pids-limit=64 \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --volume "$PWD/ops/ansible:/work:ro" --workdir /work/tests \
  --entrypoint /bin/promtool \
  quay.io/prometheus/prometheus:v3.15.0@sha256:efd719c99d83b060d9daefdcf00360461adf279f45ef5391f8d111892118753e \
  test rules monitoring-rules.test.yml
"###;
pub const CADDY_SHA256: &str = "61ea07897a7d010fdcc8dd77c028acdc93adef2f5dfeeb72b3a336bb8a7906ac";
pub const CADDY: &str = r###"# The official image marks /usr/bin/caddy with NET_BIND_SERVICE.
# Retain only that capability so Linux can execute the binary while
# the validation container remains otherwise isolated.
docker run --rm \
  --network none \
  --read-only \
  --cap-drop ALL \
  --cap-add NET_BIND_SERVICE \
  --security-opt no-new-privileges \
  --tmpfs /config:rw,nosuid,nodev,noexec,size=16m \
  --tmpfs /data:rw,nosuid,nodev,noexec,size=16m \
  --env TRUSTED_PROXY_SECRET \
  --mount type=bind,src="${PWD}/Caddyfile",dst=/etc/caddy/Caddyfile,readonly \
  --entrypoint /usr/bin/caddy \
  "${CADDY_IMAGE}" \
  validate --config /etc/caddy/Caddyfile --adapter caddyfile
"###;
