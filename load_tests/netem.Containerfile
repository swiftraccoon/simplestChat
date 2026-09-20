# NET_ADMIN sidecar that applies tc/netem inside another container's network
# namespace for impaired-network runs (see benchmark-podman.mjs).
FROM docker.io/library/alpine:3.22@sha256:2e1a7aa4cbc4e9e5222bb4c24a839aa1a6170ea5492d644777ce7b178824e44f
RUN apk add --no-cache iproute2
ENTRYPOINT ["/bin/sh"]
