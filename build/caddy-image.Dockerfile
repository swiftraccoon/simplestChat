# Preserve the reviewed official image's filesystem, entrypoint and capabilities
# while its stable upstream binary release is ahead of the official image tags.
FROM docker.io/library/caddy:2.11.6-alpine@sha256:d44355d3c2149dc580ce2cac735955d1c08d3d00882c30489c241aa51a5c10d9

ARG TARGETARCH
ARG SOURCE_REVISION
ENV CADDY_VERSION=v2.11.7

# Both official archives are checked against their published SHA-256 identities.
# BuildKit's bundled emulators run the same version/config checks on both targets.
# The bind-mounted public config and fixture secret never become image layers.
RUN --mount=type=bind,source=Caddyfile,target=/tmp/reviewed-Caddyfile,readonly \
    set -eu; \
    case "$TARGETARCH" in \
      amd64) checksum=727b91701a392de6ebc5027509f548bf39979e5216340d0faed8fa5e69c84f8b ;; \
      arm64) checksum=d8fc6d179a5d283028a472a5618564f6ad8a86fed513e64f032b3b0b7cc45e42 ;; \
      *) echo 'unsupported Caddy architecture' >&2; exit 1 ;; \
    esac; \
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      --retry 3 --connect-timeout 15 --max-time 120 \
      "https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_linux_${TARGETARCH}.tar.gz" \
      --output /tmp/caddy.tar.gz; \
    printf '%s  /tmp/caddy.tar.gz\n' "$checksum" | sha256sum -c -; \
    tar -xzf /tmp/caddy.tar.gz -C /usr/bin caddy; \
    rm /tmp/caddy.tar.gz; \
    chmod 0755 /usr/bin/caddy; \
    setcap cap_net_bind_service=+ep /usr/bin/caddy; \
    test "$(getcap /usr/bin/caddy)" = '/usr/bin/caddy cap_net_bind_service=ep'; \
    test "$(caddy version | cut -d ' ' -f 1)" = "$CADDY_VERSION"; \
    TRUSTED_PROXY_SECRET=ci-only-proxy-secret-00000000000000000000000000000000 \
      caddy validate --config /tmp/reviewed-Caddyfile --adapter caddyfile

LABEL org.opencontainers.image.version=v2.11.7 \
    org.opencontainers.image.source=https://github.com/swiftraccoon/simplestChat \
    org.opencontainers.image.revision=$SOURCE_REVISION \
    org.opencontainers.image.description="Official Caddy release binary on the pinned official Caddy base"
