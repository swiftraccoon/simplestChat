# WebRTC Client for mediasoup

Real WebRTC client using webrtc-rs 0.17 that establishes genuine ICE/DTLS/RTP connections with mediasoup.

## Files

| File | Purpose |
|------|---------|
| `webrtc_client.rs` | `WebRtcTransport` + `WebRtcSession` - PeerConnection management |
| `media_generator.rs` | RTP packet generation (Opus audio, VP8 video) |
| `metrics.rs` | Thread-safe atomic metrics collection |
| `mod.rs` | Module exports |

## How It Works

mediasoup uses parameter-based signaling (ICE params, DTLS params), while webrtc-rs uses SDP. The client bridges the gap:

1. **Transport creation**: Create webrtc-rs PeerConnection with codecs matching mediasoup
2. **DTLS extraction**: Generate SDP offer to get real DTLS fingerprint
3. **Remote SDP**: Synthesize SDP answer from mediasoup's ICE/DTLS parameters
4. **Connection**: ICE/DTLS handshake completes, RTP flows

### Key Implementation Details

**DTLS role**: Client always uses `DtlsRole::Client` for both send and recv transports. Remote SDP answer uses `a=setup:passive` (map `DtlsRole::Auto` to `passive`).

**ICE candidate format**: mediasoup enums (`Protocol`, `IceCandidateType`) have capitalized Debug format (`Udp`, `Host`), but ICE SDP requires lowercase (`udp`, `host`). Use explicit match arms.

**VP8 payload descriptor**: Must include Picture ID (X=1, I=1 bits) or mediasoup's C++ worker crashes on malformed payloads.

**MID header extension**: Register `urn:ietf:params:rtp-hdrext:sdes:mid` and include in RTP packets so webrtc-rs routes packets to correct transceivers.

**on_track handler**: Must `tokio::spawn` the read loop inside the handler so the returned future completes immediately. Otherwise the `Mutex<OnTrackHdlrFn>` lock is held forever, blocking subsequent tracks.

**Deferred consumer resume**: Resume consumers AFTER SDP renegotiation + 100ms delay. webrtc-rs enqueues `start_rtp` asynchronously during renegotiation; SSRCs aren't registered until the operation runs.

## API

### WebRtcSession

```rust
let mut session = WebRtcSession::new(client_id, metrics);

// Create transports (returns local DTLS params to send to server)
let send_dtls = session.create_send_transport(
    transport_id, ice_parameters, ice_candidates, dtls_parameters
).await?;

let recv_dtls = session.create_recv_transport(
    transport_id, ice_parameters, ice_candidates, dtls_parameters
).await?;

// Produce audio/video
let audio_track = session.produce_audio().await?;
let video_track = session.produce_video().await?;

// Add consumer (called when ConsumerCreated arrives)
session.add_consumer(consumer_id, kind, rtp_parameters).await?;

// Renegotiate after batch of consumers added
session.renegotiate_consumers().await?;
```

## Debugging

```bash
RUST_LOG=debug ./target/release/load_test --clients 1 --duration 10
```

Key log messages:
- `Transport connected` - ICE/DTLS handshake succeeded
- `on_track fired` - Receiving media from a consumer
- `Failed to set remote description` - SDP generation issue
- `ICE state: Failed` - Network connectivity problem
