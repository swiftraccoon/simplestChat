# Multi-stage Dockerfile for SimplestChat mediasoup server
# Using Fedora 42 (Red Hat-based Linux)

# Stage 1: Builder
FROM fedora:42 AS builder

# Install build dependencies for mediasoup
# KEY: libstdc++-static is required for mediasoup-sys build.rs static linking
RUN dnf install -y \
    gcc \
    gcc-c++ \
    make \
    cmake \
    python3 \
    python3-pip \
    python3-devel \
    openssl-devel \
    pkg-config \
    git \
    perl \
    which \
    libstdc++-static \
    glibc-static \
    && pip3 install meson ninja && \
    dnf clean all

# Install Rust 1.90
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.90.0 --profile minimal
ENV PATH="/root/.cargo/bin:/root/.local/bin:${PATH}"

# Set working directory
WORKDIR /app

# Copy source
COPY . .

# Build the project
RUN cargo build --release --bin simplestChat --bin load_test

# Stage 2: Runtime
FROM fedora:42

# Install minimal runtime dependencies
RUN dnf install -y --setopt=install_weak_deps=False \
    libstdc++ \
    openssl-libs \
    && dnf clean all

WORKDIR /app

# Copy built binaries
COPY --from=builder /app/target/release/simplestChat /app/simplestChat
COPY --from=builder /app/target/release/load_test /app/load_test

# Expose WebSocket port
EXPOSE 3000

# Set environment
ENV RUST_LOG=simplestChat=info,mediasoup=warn

# Run the server
CMD ["/app/simplestChat"]
