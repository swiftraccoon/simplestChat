# WebRTC Client for mediasoup

Real WebRTC client using webrtc-rs 0.20's async Sans-I/O driver that establishes genuine ICE/DTLS/RTP connections with mediasoup.

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

**Track events**: `PeerConnectionEventHandler::on_track` spawns a cancellable
`TrackRemote::poll()` loop and returns promptly so event dispatch can continue.
Only `OnRtpPacket` events contribute to received-media metrics. Local-track
feedback pollers and remote-track pollers stop when the transport closes or drops;
the peer's background driver is explicitly closed as well.

**Consumer SDP**: Each received producer gets its own transceiver and SDP media
section, with stable MID, MSID and SSRC mapping across batched renegotiations.
Consumers resume only after their receive descriptions are installed; the load
client retains a short settling delay before sending resume messages.

**Send SSRC**: Synthetic packets are parsed as RTP and rewritten to each local
track's negotiated SSRC before `write_rtp`. The new driver does not rewrite an
unmatched SSRC automatically.

**Send readiness**: `write_rtp` queues work instead of waiting for the network
handshake. Publishers wait for the connection's `Connected` event before
generating media or starting the session timer. A failed/closed transport or a
ten-second setup timeout is an error; an ICE/DTLS setup failure is not counted
as a successful media run.

**Local testing**: Loopback ICE candidates select an explicit same-family
loopback UDP bind. Other candidates use a wildcard bind of the matching family.

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
let audio_track = session.produce_audio()?;
let video_track = session.produce_video()?;

// Add consumer (called when ConsumerCreated arrives)
session.record_consumer(producer_id, kind, &rtp_parameters)?;

// Renegotiate after batch of consumers added
session.renegotiate_consumers().await?;
```

## Debugging

```bash
# Run from the repository root after installing the pinned OpenSSL build as in
# the main README.
export OPENSSL_DIR="$PWD/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$PWD/build/pip-constraints.txt"
cargo build --locked --release --features load-test --bin load_test
RUST_LOG=debug ./target/release/load_test --clients 1 --duration 10
```

Key log messages:
- `Transport connected` - ICE/DTLS handshake succeeded
- `Received ... RTP packets` - Receiving media from a consumer (periodic debug log)
- `Failed to set remote description` - SDP generation issue
- `ICE state: Failed` - Network connectivity problem
