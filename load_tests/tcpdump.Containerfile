# NET_RAW sidecar that captures STUN and DTLS handshake packets inside another
# container's network namespace for handshake diagnosis (see
# benchmark-podman.mjs --capture-handshakes).
FROM docker.io/library/alpine:3.22@sha256:2e1a7aa4cbc4e9e5222bb4c24a839aa1a6170ea5492d644777ce7b178824e44f
RUN apk add --no-cache tcpdump
ENTRYPOINT ["/usr/bin/tcpdump"]
