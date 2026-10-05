# Official multi-architecture image digests verified 2026-09-09. The package
# refresh epoch also advances for security updates between base-digest reviews.
ARG FEDORA_REFRESH_EPOCH=2026-09-30

# Node builds the frontend and projects image reviews; it is never deployed.
# Pin the latest stable Current release and its verified multi-arch manifest.
FROM docker.io/library/node:26.10.0-bookworm-slim@sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2 AS image-review-inputs
COPY security/exceptions.json /reviews.json
# Only image-relevant reviews invalidate package layers. The complete current
# policy is still validated by the independent image audit on every release.
RUN node --input-type=module - /reviews.json /image-exceptions.json <<'IMAGE_REVIEWS'
import { readFileSync, writeFileSync } from 'node:fs';
const [input, output] = process.argv.slice(2);
const policy = JSON.parse(readFileSync(input, 'utf8'));
if (policy.schemaVersion !== 1 || !Array.isArray(policy.exceptions) ||
    !policy.exceptions.every(row => row && typeof row === 'object' &&
      !Array.isArray(row) && typeof row.scanner === 'string')) {
  throw new Error('Invalid image review input');
}
const scanners = new Set(['grype', 'image-license', 'gitleaks']);
const records = policy.exceptions.filter(row => scanners.has(row.scanner))
  .map(row => JSON.stringify(Object.fromEntries(Object.keys(row).sort().map(key => [key, row[key]]))))
  .sort().map(row => JSON.parse(row));
writeFileSync(output, JSON.stringify({ schemaVersion: 1, exceptions: records }) + '\n', { mode: 0o644 });
IMAGE_REVIEWS

FROM docker.io/library/node:26.10.0-bookworm-slim@sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2 AS web-builder
COPY build/install-npm.mjs /usr/local/lib/install-simplestchat-npm.mjs
RUN node /usr/local/lib/install-simplestchat-npm.mjs /opt/npm-12.2.0
ENV PATH="/opt/npm-12.2.0/bin:${PATH}"
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --ignore-scripts
COPY web/tsconfig.json web/tsconfig.app.json web/tsconfig.tools.json web/vite.config.ts web/index.html web/bundle-budget.json ./
COPY web/scripts/check-bundle.mjs web/scripts/mediasoup-runtime.mjs ./scripts/
COPY web/public ./public
COPY web/src ./src
ARG SOURCE_REVISION=unknown
ENV FRONTEND_REVISION=${SOURCE_REVISION}
RUN npm run build

# Build on the same supported Fedora release used at runtime. mediasoup-sys
# requires a C++ toolchain and the static C/C++ runtime libraries.
FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS builder
ARG FEDORA_REFRESH_EPOCH
# Relevant public reviews invalidate package caches without entering image layers.
RUN --mount=type=bind,from=image-review-inputs,source=/image-exceptions.json,target=/tmp/simplestchat-image-exceptions.json,readonly \
    --mount=type=bind,source=security/image-policy.json,target=/tmp/simplestchat-image-policy.json,readonly \
    test -n "${FEDORA_REFRESH_EPOCH}" \
    && test -s /tmp/simplestchat-image-exceptions.json \
    && test -s /tmp/simplestchat-image-policy.json \
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
# current stable release from its official, checksum-pinned source so both the C++
# worker and Rust OpenSSL bindings use one version and the runtime cannot load
# an older libssl with the same SONAME.
COPY build/install-openssl.sh /usr/local/bin/install-simplestchat-openssl
RUN /usr/local/bin/install-simplestchat-openssl /opt/openssl-4.0.3
ENV OPENSSL_DIR=/opt/openssl-4.0.3 \
    OPENSSL_STATIC=1 \
    PKG_CONFIG_PATH=/opt/openssl-4.0.3/lib/pkgconfig

# Verify rustup before executing it. TARGETARCH is supplied by Docker/BuildKit;
# the uname fallback also supports direct Podman builds.
ARG TARGETARCH
RUN set -eu; \
    build_arch="${TARGETARCH:-$(uname -m)}"; \
    case "${build_arch}" in \
        amd64|x86_64) \
            rust_arch="x86_64-unknown-linux-gnu"; \
            rustup_sha256="dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71" \
            ;; \
        arm64|aarch64) \
            rust_arch="aarch64-unknown-linux-gnu"; \
            rustup_sha256="15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433" \
            ;; \
        *) echo "unsupported build architecture: ${build_arch}" >&2; exit 1 ;; \
    esac; \
    curl --fail --show-error --location --proto '=https' --tlsv1.2 \
        "https://static.rust-lang.org/rustup/archive/1.29.1/${rust_arch}/rustup-init" \
        --output /tmp/rustup-init; \
    echo "${rustup_sha256}  /tmp/rustup-init" | sha256sum --check --strict; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --default-toolchain 1.99.0 --profile minimal \
        --component rustfmt; \
    rm /tmp/rustup-init
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
# Authenticate the exact cargo-auditable executable before either Cargo build.
# Its dependency section must survive in the final executable for image auditing.
COPY build/security_tools.py build/security-tools.lock.json ./build/
RUN python3 build/security_tools.py install --tools cargo-auditable --directory /opt/security-tools
# The toolchain and build-required rustfmt component are installed above. Do not
# copy the developer toolchain file: its IDE-only components would enter this layer.
COPY build/pip-constraints.txt /opt/simplestchat/pip-constraints.txt
ENV PIP_CONSTRAINT=/opt/simplestchat/pip-constraints.txt
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY build/security_vendor.py ./build/
COPY ops/ansible/files/bounded_process.py ops/ansible/files/release_json.py ./ops/ansible/files/
RUN python3 build/security_vendor.py verify --cache /tmp/vendor-cache --output /app/vendor-evidence

# Warm every dependency, including the native worker, against stub sources.
# This layer is reused until the manifest, lockfile or vendored patches change,
# so a source-only build compiles this crate alone instead of the whole graph.
RUN mkdir -p src load_tests/bin \
    && printf '#![allow(non_snake_case)]\nfn main() {}\n' > src/main.rs \
    && printf '#![allow(non_snake_case)]\n' > src/lib.rs \
    && printf 'fn main() {}\n' > load_tests/bin/load_test.rs \
    && /opt/security-tools/bin/cargo-auditable auditable build --locked --release --bin simplestChat \
    && rm -rf src load_tests \
        target/release/simplestChat \
        target/release/deps/simplestChat-* target/release/deps/libsimplestChat-* \
        target/release/.fingerprint/simplestChat-*
COPY src ./src
COPY migrations/*.sql ./migrations/
RUN python3 build/security_tools.py path cargo-auditable --directory /opt/security-tools \
    && /opt/security-tools/bin/cargo-auditable auditable build --locked --release --bin simplestChat \
        --message-format=json > /app/cargo-build.json \
    && strings target/release/simplestChat | grep -Fq 'OpenSSL 4.0.3 29 Sep 2026' \
    && ! strings target/release/simplestChat | grep -Fq 'OpenSSL 3.0.8' \
    && ! ldd target/release/simplestChat | grep -Eq 'lib(ssl|crypto)\.so'
COPY build/security_native.py build/security_elf.py build/install-openssl.sh ./build/
RUN python3 build/security_native.py --root /app \
    --vendor-report /app/vendor-evidence/report.json \
    --cargo-messages /app/cargo-build.json --cargo-home /root/.cargo \
    --openssl-prefix /opt/openssl-4.0.3 --output /app/native-components.build.json

# The load tester has a separate target so its WebRTC client dependencies and
# executable are absent from the default production image.
FROM builder AS loadtest-builder
COPY load_tests ./load_tests
RUN cargo build --locked --release --features load-test --bin load_test

FROM docker.io/library/fedora:44@sha256:43b29f65a41eb9c35e1cd5323e3bdf3b655c2357a9f4f1ff2f9c2798e5045d80 AS runtime-base
ARG FEDORA_REFRESH_EPOCH
RUN --mount=type=bind,from=image-review-inputs,source=/image-exceptions.json,target=/tmp/simplestchat-image-exceptions.json,readonly \
    --mount=type=bind,source=security/image-policy.json,target=/tmp/simplestchat-image-policy.json,readonly \
    test -n "${FEDORA_REFRESH_EPOCH}" \
    && test -s /tmp/simplestchat-image-exceptions.json \
    && test -s /tmp/simplestchat-image-policy.json \
    && dnf upgrade -y --refresh \
    && dnf install -y --setopt=install_weak_deps=False \
    ca-certificates \
    libstdc++ \
    openssl-libs \
    shadow-utils \
    && groupadd --gid 10001 simplestchat \
    && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent \
        --shell /sbin/nologin simplestchat \
    && dnf clean all \
    && rm -f /etc/ld.so.cache /etc/nsswitch.conf
# Keep runtime name lookup within glibc's built-in files/DNS implementations.
# Replace the base image's authselect symlink before COPY can follow it.
# The image audit verifies these bytes and default-directory library resolution.
COPY security/runtime/nsswitch.conf /etc/nsswitch.conf

WORKDIR /app
ARG SOURCE_REVISION=unknown
ENV SOURCE_REVISION=${SOURCE_REVISION}
COPY --from=builder /app/target/release/simplestChat /app/simplestChat
COPY --from=builder /app/native-components.build.json /usr/share/simplestchat/native-components.json
COPY --from=builder /app/migrations /app/migrations
COPY --from=builder /app/vendor/seclists-passwords/LICENSE /app/vendor/seclists-passwords/README.md /usr/share/licenses/simplestchat/seclists/
COPY --from=web-builder /web/dist /app/web/dist
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
