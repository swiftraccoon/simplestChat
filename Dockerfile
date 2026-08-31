# Official multi-architecture image digests verified 2026-08-30. Refresh the
# digest pins and this package-cache epoch together on the documented cadence.
ARG FEDORA_REFRESH_EPOCH=2026-08-30

# Build the browser client with a supported Node.js LTS release.
FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --ignore-scripts
COPY web/tsconfig.json web/vite.config.ts web/index.html ./
COPY web/src ./src
RUN npm run build

# Build on the same supported Fedora release used at runtime. mediasoup-sys
# requires a C++ toolchain and the static C/C++ runtime libraries.
FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS builder
ARG FEDORA_REFRESH_EPOCH
RUN test -n "${FEDORA_REFRESH_EPOCH}" \
    && dnf upgrade -y --refresh \
    && dnf install -y --setopt=install_weak_deps=False \
    ca-certificates \
    cmake \
    curl \
    gcc \
    gcc-c++ \
    git \
    glibc-static \
    libstdc++-static \
    make \
    meson \
    ninja-build \
    perl \
    pkgconf-pkg-config \
    python3 \
    python3-devel \
    python3-pip \
    which \
    && dnf clean all

# Fedora 44 currently has no native OpenSSL static-devel package. Build the
# fixed LTS release from its official, checksum-pinned source so both the C++
# worker and Rust OpenSSL bindings use one version and the runtime cannot load
# an older libssl with the same SONAME.
COPY build/install-openssl.sh /usr/local/bin/install-simplestchat-openssl
RUN /usr/local/bin/install-simplestchat-openssl /opt/openssl-3.5.8
ENV OPENSSL_DIR=/opt/openssl-3.5.8 \
    OPENSSL_STATIC=1 \
    PKG_CONFIG_PATH=/opt/openssl-3.5.8/lib/pkgconfig

# Verify rustup before executing it. TARGETARCH is supplied by Docker/BuildKit;
# the uname fallback also supports direct Podman builds.
ARG TARGETARCH
RUN set -eu; \
    build_arch="${TARGETARCH:-$(uname -m)}"; \
    case "${build_arch}" in \
        amd64|x86_64) \
            rust_arch="x86_64-unknown-linux-gnu"; \
            rustup_sha256="20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c" \
            ;; \
        arm64|aarch64) \
            rust_arch="aarch64-unknown-linux-gnu"; \
            rustup_sha256="e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c" \
            ;; \
        *) echo "unsupported build architecture: ${build_arch}" >&2; exit 1 ;; \
    esac; \
    curl --fail --show-error --location --proto '=https' --tlsv1.2 \
        "https://static.rust-lang.org/rustup/archive/1.28.2/${rust_arch}/rustup-init" \
        --output /tmp/rustup-init; \
    echo "${rustup_sha256}  /tmp/rustup-init" | sha256sum --check --strict; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --default-toolchain 1.98.0 --profile minimal \
        --component rustfmt; \
    rm /tmp/rustup-init
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
# The toolchain and build-required rustfmt component are installed above. Do not
# copy the developer toolchain file: its IDE-only components would enter this layer.
COPY build/pip-constraints.txt /opt/simplestchat/pip-constraints.txt
ENV PIP_CONSTRAINT=/opt/simplestchat/pip-constraints.txt
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY src ./src
COPY migrations/*.sql ./migrations/
RUN grep -Fq 'source_filename = abseil-cpp-20240722.2.tar.gz' \
        vendor/mediasoup-sys-0.17.0/subprojects/abseil-cpp.wrap \
    && grep -Fq 'source_hash = ec820b01d9b328ca1f1b9c4e5b305d7a9fa03dc410ef64ba6654b637f9a4c3a8' \
        vendor/mediasoup-sys-0.17.0/subprojects/abseil-cpp.wrap \
    && grep -Fq 'patch_directory = abseil-cpp' \
        vendor/mediasoup-sys-0.17.0/subprojects/abseil-cpp.wrap \
    && echo '454b10520ba4ba4a9995612ba2d9e6490b5477bb5093eff54af4c84917f71f19  vendor/mediasoup-sys-0.17.0/subprojects/packagefiles/abseil-cpp/meson.build' \
        | sha256sum --check --strict \
    && echo '7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb  vendor/mediasoup-sys-0.17.0/subprojects/packagefiles/abseil-cpp/LICENSE.build' \
        | sha256sum --check --strict \
    && cargo build --locked --release --bin simplestChat \
    && strings target/release/simplestChat | grep -Fq 'OpenSSL 3.5.8 25 Aug 2026' \
    && ! strings target/release/simplestChat | grep -Fq 'OpenSSL 3.0.8' \
    && ! ldd target/release/simplestChat | grep -Eq 'lib(ssl|crypto)\.so'

# The load tester has a separate target so its WebRTC client dependencies and
# executable are absent from the default production image.
FROM builder AS loadtest-builder
COPY load_tests ./load_tests
RUN cargo build --locked --release --features load-test --bin load_test

FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS runtime-base
ARG FEDORA_REFRESH_EPOCH
RUN test -n "${FEDORA_REFRESH_EPOCH}" \
    && dnf upgrade -y --refresh \
    && dnf install -y --setopt=install_weak_deps=False \
    ca-certificates \
    libstdc++ \
    openssl-libs \
    shadow-utils \
    && groupadd --gid 10001 simplestchat \
    && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent \
        --shell /sbin/nologin simplestchat \
    && dnf clean all

WORKDIR /app
COPY --from=builder --chown=10001:10001 /app/target/release/simplestChat /app/simplestChat
COPY --from=builder --chown=10001:10001 /app/migrations /app/migrations
COPY --from=web-builder --chown=10001:10001 /web/dist /app/web/dist
RUN ldd /app/simplestChat > /tmp/simplestchat-ldd \
    && ! grep -Fq 'not found' /tmp/simplestchat-ldd \
    && rm /tmp/simplestchat-ldd

USER 10001:10001
EXPOSE 3000
ENV RUST_LOG=simplestChat=info,mediasoup=warn
CMD ["/app/simplestChat"]

FROM runtime-base AS loadtest
COPY --from=loadtest-builder --chown=10001:10001 /app/target/release/load_test /app/load_test
USER root
RUN ldd /app/load_test > /tmp/load-test-ldd \
    && ! grep -Fq 'not found' /tmp/load-test-ldd \
    && rm /tmp/load-test-ldd \
    && mkdir /results \
    && chown 10001:10001 /results
USER 10001:10001
WORKDIR /results
ENTRYPOINT ["/app/load_test"]
CMD ["--help"]

# Keep this final so an unqualified build produces the least-privileged server
# image, not the load-testing image.
FROM runtime-base AS production
