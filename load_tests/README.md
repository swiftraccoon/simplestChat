# Load Testing Infrastructure

Real WebRTC load testing for the SimplestChat mediasoup server. Each synthetic client establishes genuine ICE/DTLS/RTP connections using webrtc-rs 0.17.

## Components

### WebRTC Client (`clients/webrtc_client.rs`)
- Full WebRTC PeerConnection per transport (send + receive)
- Real ICE/DTLS handshake with mediasoup
- Sends VP8 video and Opus audio RTP packets
- Receives media from all other participants via consumers
- See `clients/README.md` for implementation details

### Synthetic Client (`clients/synthetic_client.rs`)
- WebSocket signaling protocol implementation
- Room join, transport creation, producer/consumer lifecycle
- Message handling (NewProducer, ParticipantJoined, ConsumerCreated, etc.)

### Media Generator (`clients/media_generator.rs`)
- Generates synthetic RTP packets with valid VP8/Opus payloads
- Audio: Opus, 48kHz stereo, 20ms packets (50 pkt/s)
- Video: VP8, 30fps, keyframes every 5s, with proper VP8 payload descriptor

### Metrics Collector (`clients/metrics.rs`)
- Thread-safe atomic counters (no locks)
- Tracks: connection time, packets/bytes sent/received, producers, consumers, errors
- Generates per-client reports and aggregate summaries with percentiles (P50/P95/P99)

## Running Load Tests

### Command Line

```bash
./target/release/load_test \
  --server ws://localhost:3000/ws \
  --clients 10 \
  --duration 30 \
  --ramp-up 5 \
  --room my-test-room
```

### Options

```
-c, --clients <N>       Number of concurrent clients (default: 5)
-d, --duration <SECS>   Session duration in seconds (default: 30)
-r, --ramp-up <SECS>    Ramp-up period in seconds (default: 5)
-s, --server <URL>      Server WebSocket URL (default: ws://localhost:3000/ws)
--room <ID>             Room ID to join (default: load-test-room)
--audio-only            Send only audio (no video)
--video-only            Send only video (no audio)
```

### Using the Stress Test Script (Recommended)

The stress test script handles server restart, resource monitoring, and result collection:

```bash
# Progressive test: 10, 50, 100 clients (30s each)
./scripts/stress-test.sh "10 50 100" 30

# Large scale test
./scripts/stress-test.sh "100 300 500 750 1000" 30
```

This automatically:
- Restarts the server between test levels (clean state)
- Sets `ulimit -n 65536` for server and load test
- Scales ramp-up time with client count
- Monitors server CPU/memory/FDs
- Prints summary table at end

## Output

### `load_test_results.json` - Per-client metrics
```json
[
  {
    "clientId": "client-0",
    "connectionSuccessful": true,
    "connectionTimeMs": 245,
    "totalPacketsSent": 1500,
    "totalPacketsReceived": 3000,
    "totalBytesSent": 120000,
    "totalBytesReceived": 240000,
    "errors": [],
    "producersCreated": 2,
    "consumersCreated": 18
  }
]
```

### `load_test_summary.json` - Aggregate summary
```json
{
  "totalClients": 10,
  "successfulConnections": 10,
  "totalErrors": 0,
  "totalPacketsSent": 15000,
  "totalPacketsReceived": 30000,
  "totalProducersCreated": 20,
  "totalConsumersCreated": 180
}
```

## Client Flow

```
1. WebSocket connect → ws://server:3000/ws
2. JoinRoom → RoomJoined
3. GetRouterRtpCapabilities → RouterRtpCapabilities
4. CreateSendTransport → TransportCreated (with ICE/DTLS params)
5. Create webrtc-rs PeerConnection, extract real DTLS fingerprint
6. ConnectTransport (real DTLS params) → TransportConnected
7. CreateRecvTransport → same flow
8. Produce (audio + video) → ProducerCreated
9. Start sending RTP packets through webrtc-rs
10. Handle ConsumerCreated events → SDP renegotiation → ResumeConsumer
11. Receive RTP packets via on_track handler
12. Run for session duration, then disconnect
```

### Consumer Renegotiation Batching

For large rooms, many ConsumerCreated events arrive rapidly. The client batches them:
1. Handle each ConsumerCreated (add consumer to PeerConnection)
2. Drain all pending WebSocket messages via `now_or_never()`
3. Single SDP renegotiation for the entire batch
4. Resume all consumers after renegotiation + brief delay

This reduces setup time from O(N * 150ms) to O(1) per batch.

## Key Metrics to Watch

| Metric | Good | Warning | Critical |
|--------|------|---------|----------|
| Connection success | 100% | >95% | <90% |
| Errors | 0 | <5 | >10 |
| Packet recv/send ratio | >40% | >20% | <10% |
| Server CPU | <70% | <85% | >90% |
| Server RSS | <1GB | <2GB | >4GB |

## Troubleshooting

### "Too many open files"
Both server and load test need `ulimit -n 65536`. Each client uses ~3 FDs (WebSocket + 2 PeerConnections).

### Low packet receive ratio
Normal during ramp-up: early clients don't consume from late clients. The ratio improves with longer test durations relative to ramp-up.

### Connection timeouts
- Check server is running: `./scripts/server-status.sh`
- Reduce clients or increase ramp-up
- Check server logs on the remote host
