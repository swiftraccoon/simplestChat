# SimplestChat - Project Goals & Architecture

## Primary Objective

Build a highly efficient, real-time communication platform supporting text chat, audio, and video within user-created rooms, maximizing performance and minimizing resource utilization.

## Core Technology Stack

### Backend: Rust + mediasoup

- **Rust** for all application logic (signaling, room management, business logic)
- **mediasoup (Rust crate)** for WebRTC SFU (Selective Forwarding Unit)
- **mediasoup C++ workers** for media packet routing
- **Rationale**: Maximum efficiency by leveraging Rust's zero-cost abstractions and C++'s raw performance for media handling

### Architecture Philosophy

**Use Rust/C++ everywhere possible** to maximize:

- CPU efficiency (no garbage collection, minimal runtime overhead)
- Memory efficiency (predictable allocations, no GC pauses)
- Network efficiency (optimal packet handling, minimal overhead)
- Concurrency performance (Rust's async/await + tokio runtime)

## Critical Success Criterion: Testing Framework

### Priority #1: Comprehensive Load Testing

Before feature development, we must establish a **robust, automated testing framework** capable of:

#### 1. Synthetic Load Generation

- Programmatically spawn N simulated clients (100, 500, 1000+)
- Each simulated client can:
  - Join/leave rooms
  - Publish audio/video streams (synthetic media)
  - Receive streams from other participants
  - Send text chat messages

#### 2. Real-World Simulation Scenarios

- **Scenario A**: Conference room (10-50 participants, all with video)
- **Scenario B**: Webinar (1 presenter, 100-1000 viewers)
- **Scenario C**: Audio-only rooms (lower bandwidth requirements)
- **Scenario D**: Churn test (participants joining/leaving rapidly)
- **Scenario E**: Network instability (simulated packet loss, jitter, bandwidth constraints)

#### 3. Metrics Collection & Analysis

Must measure and report:

**Server-Side Metrics:**

- CPU utilization (overall + per mediasoup worker)
- Memory usage (heap, RSS)
- Network bandwidth (ingress/egress per room, per participant)
- Connection count (active transports, producers, consumers)
- Packet loss rates
- Latency (signaling response time, media latency)

**Client-Side Metrics (from simulated clients):**

- Connection success rate
- Time to first frame
- Audio/video quality received
- Bandwidth consumption per client
- Connection stability (reconnection events)
- ICE connection times

**Scalability Metrics:**

- Max concurrent participants per room
- Max concurrent rooms per server
- Bandwidth saturation point
- CPU saturation point
- Memory saturation point

#### 4. Testing Tools to Implement/Integrate

**Option 1: Custom Rust Load Testing Framework**

- Build synthetic WebRTC clients in Rust using `webrtc-rs` or `libmediasoupclient` bindings
- Full control, maximum efficiency
- Can run distributed across multiple load-generation machines

**Option 2: Existing Tools + Custom Orchestration**

- `mediasoup-client` in Node.js (headless clients)
- `Puppeteer`/`Playwright` for browser automation (more realistic but heavier)
- Custom Rust orchestrator to manage test execution

**Option 3: Hybrid Approach**

- Rust-based signaling clients (lightweight, thousands of connections)
- Selective real browser instances for validation
- Prometheus + Grafana for metrics visualization

#### 5. Continuous Performance Benchmarking

- Automated nightly load tests
- Performance regression detection
- Historical trend analysis
- Alert on degradation thresholds

## Technical Requirements

### Efficiency Optimizations to Implement

1. **Simulcast** - Clients send multiple quality layers (1080p, 720p, 360p)
2. **SVC (Scalable Video Coding)** - Single stream, multiple quality layers
3. **Dynamic layer selection** - Server selects appropriate quality per consumer
4. **Bandwidth estimation** - Adaptive bitrate based on network conditions
5. **Selective subscription** - Only forward streams actively being viewed
6. **Voice activity detection** - Pause video encoding during audio-only
7. **Efficient codec selection** - VP9 SVC or AV1 for best compression

### Infrastructure Considerations

- Single Rust process with multiple mediasoup workers (one per CPU core)
- Horizontal scaling plan: Multiple servers + Redis for room coordination (future)
- Target: 100-500 concurrent users per server initially
- Stretch goal: 1000+ concurrent users with optimizations

## Development Phases

### Phase 0: Foundation & Testing Infrastructure (CRITICAL)

**Must complete before feature development**

1. Initialize Rust project with mediasoup
2. Minimal signaling server (WebSocket + room join)
3. Basic mediasoup integration (create router, transports)
4. **Build load testing framework**
   - Synthetic client implementation
   - Metrics collection system
   - Test scenario runner
   - Reporting/visualization
5. **Validate** - Prove we can handle target load

### Phase 1: Core Communication Features

(Only start after Phase 0 validation)

1. Text chat (WebSocket messages)
2. Audio-only rooms
3. Video streams
4. Room creation/management UI

### Phase 2: Optimization

1. Implement simulcast/SVC
2. Bandwidth adaptation
3. Connection recovery
4. UI for quality monitoring

### Phase 3: Production Readiness

1. Authentication/authorization
2. Persistence (room history, user data)
3. Deployment automation
4. Monitoring/alerting

## Success Metrics

### Technical Goals

- [x] Support 500+ concurrent users on single server (4-8 core machine) — **1000 validated, 0 errors**
- [ ] < 200ms signaling latency (99th percentile)
- [ ] < 1% packet loss under normal conditions
- [ ] < 500KB/s bandwidth per participant (with adaptive bitrate)
- [x] < 50MB memory per participant — **~1MB per client at 1000 clients**
- [x] 99.9% connection success rate — **100% at 1000 clients**

### Testing Goals

- [ ] Automated load tests running daily
- [x] Load test suite covering all scenarios — stress-test.sh with progressive scaling
- [x] Performance benchmarks tracked over time — results saved to results/stress_test/
- [ ] < 5% performance regression tolerance

## Why Testing First?

**Reasoning:**

1. **Avoid costly rewrites** - Discover scaling bottlenecks early
2. **Data-driven decisions** - Optimize based on measurements, not assumptions
3. **Confidence** - Know exact capacity before launch
4. **Continuous validation** - Catch regressions immediately
5. **Architecture validation** - Prove Rust+mediasoup meets requirements

## Questions to Answer Through Testing

- What's the practical limit of participants per room?
- Does bandwidth scale linearly with participants?
- At what point does CPU become the bottleneck vs bandwidth?
- How does simulcast affect resource usage?
- What's the impact of network instability on user experience?
- Can we handle rapid participant churn?

---

**Phase 0 Status: COMPLETE**

Validated 1000 concurrent clients with real WebRTC (ICE/DTLS/RTP) on AMD Ryzen 9 8945HS. Server CPU plateaus at ~70%, ~1MB/client memory. See `README.md` for full performance data.
