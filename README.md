# SimplestChat

A WebRTC SFU (Selective Forwarding Unit) server built with Rust and mediasoup, designed for real-time audio/video communication rooms.

## Stack

- **Rust 1.90** - Application logic, signaling, room management
- **mediasoup 0.20** - C++ media workers for RTP packet routing
- **Axum 0.8** - WebSocket signaling server
- **tokio** - Async runtime
- **webrtc-rs 0.17** - Load test client with real ICE/DTLS/RTP

## Architecture

```
Client (WebSocket) ──► Axum Signaling Server
                           │
                     ┌─────┴─────┐
                     │ RoomManager│
                     └─────┬─────┘
                           │
                     ┌─────┴──────┐
                     │ MediaServer │
                     └─────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
         WorkerMgr    RouterMgr   TransportMgr
              │            │            │
        16 C++ workers  1 per room   Send/Recv per
        (1 per core)                 participant
```

### Key Components

| Component | File | Purpose |
|-----------|------|---------|
| Signaling | `src/signaling/connection.rs` | WebSocket message handling |
| Rooms | `src/room/mod.rs` | Room lifecycle, participant management |
| Media | `src/media/mod.rs` | MediaServer orchestrator |
| Workers | `src/media/worker_manager.rs` | mediasoup C++ worker pool |
| Routers | `src/media/router_manager.rs` | Per-room media routing |
| Transports | `src/media/transport_manager.rs` | WebRTC transport + producer/consumer management |

## Performance

Validated with progressive stress testing (AMD Ryzen 9 8945HS, 8C/16T, 90GB RAM):

| Clients | Connected | Errors | CPU% | RSS (MB) | FDs |
|---------|-----------|--------|------|-----------|-----|
| 100 | 100 | 0 | 60.2 | 305 | 556 |
| 300 | 300 | 0 | 67.9 | 591 | 1538 |
| 750 | 750 | 1 | 69.4 | 812 | 2374 |
| 1000 | 1000 | 0 | 70.1 | 890 | 3124 |

- ~1 MB per client, CPU plateaus at ~70% (distributed across 16 mediasoup workers)
- Consumer scaling: N*(N-1)*2 in a full-mesh room (1000 clients = ~150K consumers)
- Requires `ulimit -n 65536` for large tests

## Quick Start

### Prerequisites

- Rust 1.90+ with nightly toolchain
- Linux x86_64 (macOS cannot build mediasoup-sys)
- `libstdc++-static`, `glibc-static`, `cmake`, `python3-pip`, `meson`, `ninja-build`

### Build & Run

```bash
# Build server + load test
cargo build --release

# Start server
ANNOUNCE_IP=<your-ip> RUST_LOG=simplestChat=info,mediasoup=warn \
  ./target/release/simplestChat

# Run load test (2 clients, 30s)
./target/release/load_test \
  --server ws://localhost:3000/ws \
  --clients 2 --duration 30
```

### Using Scripts

The `scripts/` directory contains deployment and testing helpers. Copy `scripts/` examples and customize server IPs/paths for your environment.

```bash
# Build and run locally
cargo build --release
ANNOUNCE_IP=127.0.0.1 ./target/release/simplestChat
```

## Load Testing

The load test binary (`load_tests/bin/load_test.rs`) creates real WebRTC clients using webrtc-rs that establish genuine ICE/DTLS/RTP connections with mediasoup. Each client:

1. Connects via WebSocket and joins a room
2. Creates send + receive WebRTC transports with real DTLS certificates
3. Produces audio + video tracks (synthetic RTP packets with valid VP8/Opus payloads)
4. Consumes media from all other participants
5. Collects per-client metrics (connection time, packets sent/received, errors)

Output: `load_test_results.json` (per-client) and `load_test_summary.json` (aggregate).

See `load_tests/README.md` for full documentation.

## Project Structure

```
src/
├── main.rs                    # Server entry point
├── turn.rs                    # TURN credential generation
├── signaling/
│   ├── mod.rs                 # HTTP routes, /health, WS upgrade
│   ├── connection.rs          # WebSocket message handler
│   └── protocol.rs            # Client/Server message types
├── media/
│   ├── mod.rs                 # MediaServer orchestrator
│   ├── worker_manager.rs      # mediasoup worker pool
│   ├── router_manager.rs      # Per-room routers
│   └── transport_manager.rs   # Transports, producers, consumers
└── room/
    └── mod.rs                 # Room management + broadcasts

load_tests/
├── bin/load_test.rs           # Load test binary
└── clients/
    ├── webrtc_client.rs       # Real WebRTC client (webrtc-rs)
    ├── synthetic_client.rs    # Signaling client
    ├── media_generator.rs     # RTP packet generation
    └── metrics.rs             # Atomic metrics collection

scripts/                       # Deployment, server management, testing
```

## Configuration

| Env Var | Default | Purpose |
|---------|---------|---------|
| `ANNOUNCE_IP` | `127.0.0.1` | Server's public IP for ICE candidates |
| `PORT` | `3000` | HTTP/WebSocket listen port |
| `RUST_LOG` | `info` | Log level filter |
| `TURN_URLS` | (none) | Comma-separated TURN server URLs |
| `TURN_SECRET` | (none) | TURN shared secret for credentials |
| `TURN_TTL` | `86400` | TURN credential TTL in seconds |
| RTC ports | 10000-59999 | mediasoup UDP port range |
| Workers | num_cpus | mediasoup C++ worker count |

## Server Hardening

- **Input validation**: Room ID (1-128 chars), participant name (1-64 chars), chat (1-4096 chars)
- **WebSocket size limit**: 64KB max message size prevents OOM attacks
- **Lock poisoning recovery**: All std::sync locks recover from panics instead of cascading
- **Error responses**: All operations return errors when not in a room (no silent drops)
- **Health endpoint**: `GET /health` returns `{"status":"ok","rooms":N,"participants":N}`
- **Graceful shutdown**: Ctrl+C drains all rooms, closes transports/FDs, removes routers
- **Panic-free startup**: Invalid `ANNOUNCE_IP` returns an error instead of panicking

## Known Issues

- **macOS cannot build mediasoup-sys** (`_LIBCPP_ENABLE_ASSERTIONS` error) - build on Linux
- **Container io_uring warnings** - harmless, uses epoll fallback
- **File descriptor limits** - raise `ulimit -n 65536` for 300+ clients
