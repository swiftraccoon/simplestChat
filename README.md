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
| Signaling | `src/signaling/connection.rs` | WebSocket message handling, event-driven BWE stats |
| Rooms | `src/room/mod.rs` | Room lifecycle, participant management |
| Media | `src/media/mod.rs` | MediaServer orchestrator |
| Workers | `src/media/worker_manager.rs` | Worker pool, WebRtcServer per worker, load-aware selection |
| Routers | `src/media/router_manager.rs` | Per-room media routing |
| Transports | `src/media/transport_manager.rs` | WebRTC transport, producers, consumers, BWE subscription |

## Performance

Validated with progressive stress testing (AMD Ryzen 9 8945HS, 8C/16T, 90GB RAM).
WebRtcServer shared ports, event-driven BWE, load-aware worker selection, 4+4 consumer caps:

| Clients | Rooms | P99 Latency | Errors | Consumers | FDs |
|---------|-------|-------------|--------|-----------|-----|
| 100 | 1 | 2ms | 0 | 800 | — |
| 1000 | 16 | 4ms | 0 | 8K | 1143 |
| 5000 | 16 | 84ms | 0 | 40K | 5143 |
| 10000 | 16 | 274ms | 0 | 80K | ~10K |

- **Comfortable limit**: 10000 clients/server (P99 < 300ms)
- ~1 MB per client, CPU distributed across 16 mediasoup workers
- WebRtcServer 5-tuple DEMUX eliminates per-transport file descriptors
- Requires `ulimit -n 65536` for large tests

## Quick Start

### Prerequisites

- Rust 1.90+ (stable)
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
├── metrics.rs                 # Prometheus metrics (AtomicU64, histogram)
├── turn.rs                    # TURN credential generation
├── signaling/
│   ├── mod.rs                 # HTTP routes, /health, /metrics, WS upgrade
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
    ├── media_generator.rs     # RTP packet generation
    └── metrics.rs             # Atomic metrics collection

scripts/                       # Deployment, server management, testing
```

## Configuration

| Env Var | Default | Purpose |
|---------|---------|---------|
| `ANNOUNCE_IP` | auto-detect (fallback `127.0.0.1`) | Server's public IP for ICE candidates |
| `PORT` | `3000` | HTTP/WebSocket listen port |
| `MAX_CONNECTIONS` | `10000` | Max concurrent WebSocket connections |
| `MAX_CONSUMERS_PER_PARTICIPANT` | `16` | Server-side consumer cap per participant |
| `METRICS_TOKEN` | (none) | Bearer token for `/metrics` endpoint |
| `RUST_LOG` | `simplestChat=info,mediasoup=warn` | Tracing filter |
| `TURN_URLS` | (none) | Comma-separated TURN server URLs |
| `TURN_SECRET` | (none) | TURN shared secret for credentials |
| `TURN_TTL` | `86400` | TURN credential TTL in seconds |

Hardcoded in `src/media/config.rs`:
- **RTC ports**: 10000-59999 (mediasoup UDP range)
- **Workers**: `num_cpus::get()` (1 per core)

## Server Hardening

- **Input validation**: Room ID (1-128 chars), participant name (1-64 chars), chat (1-4096 chars)
- **WebSocket size limit**: 64KB max message size prevents OOM attacks
- **Lock poisoning recovery**: All std::sync locks recover from panics instead of cascading
- **Error responses**: All operations return errors when not in a room (no silent drops)
- **Health endpoint**: `GET /health` returns `{"status":"ok","rooms":N,"participants":N}`
- **Graceful shutdown**: Ctrl+C drains all rooms, closes transports/FDs, removes routers
- **Panic-free startup**: Invalid `ANNOUNCE_IP` returns an error instead of panicking

## Efficiency & Protection

- **Bounded channels**: 256-message capacity per client prevents OOM from slow consumers
- **Connection cap**: Semaphore-based limit (default 10K), returns HTTP 503 when exhausted
- **Rate limiting**: Token-bucket per connection (100 msg/s burst), prevents message flooding
- **Idle timeout**: 5-minute WS idle timeout prevents Slowloris-style resource exhaustion
- **Reconnect tokens**: Session hijacking prevention via per-session UUIDv4 tokens

## Observability

- **`GET /metrics`**: Prometheus text exposition format, scrapable by Prometheus/Grafana
  - Counters: connections, messages sent/received, errors, rooms created, joins, leaves, producers, consumers
  - Gauges: active connections, active rooms, active participants
  - Histogram: message handling latency with 10 buckets (1ms to 5s)
- **Bearer auth**: Set `METRICS_TOKEN` env var to require `Authorization: Bearer <token>`
- **Zero dependencies**: Uses `std::sync::atomic::AtomicU64` — no prometheus/opentelemetry crates

## Known Issues

- **macOS cannot build mediasoup-sys** (`_LIBCPP_ENABLE_ASSERTIONS` error) - build on Linux
- **Container io_uring warnings** - harmless, uses epoll fallback
- **File descriptor limits** - raise `ulimit -n 65536` for 300+ clients
