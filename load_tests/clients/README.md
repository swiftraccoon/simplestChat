# WebRTC load-test client

The client bridges mediasoup's parameter-based signaling to webrtc-rs's SDP API.
It uses real ICE/DTLS/SRTP connections with synthetic audio/video payloads.
For commands, options, and report definitions, see the
[load-test README](../README.md).

## Files

| File | Responsibility |
| --- | --- |
| `webrtc_client.rs` | `WebRtcSession`, peer connections, SDP, track readers, cleanup |
| `media_generator.rs` | Synthetic Opus/VP8 RTP packets |
| `metrics.rs` | Counters, signaling histograms, diagnostic collection |
| `measurement.rs` | Shared interval, per-attempt readiness, consumer delivery checks |
| `mod.rs` | Module exports |

## How It Works

Each session creates separate send/receive peers, extracts local DTLS parameters
from an offer, and synthesizes remote SDP from the server's ICE/DTLS parameters.
Consumers are recorded in batches, installed through receive renegotiation, then
resumed through signaling.

Keep these invariants when changing the client:

- Use the client DTLS role and passive remote setup; serialize ICE candidate
  protocol/type names in lowercase.
- Give each consumer a transceiver/media section with stable MID and SSRC
  mappings across renegotiations. Install descriptions before resuming consumers;
  failed renegotiation must end signaling receipt without resuming the failed batch.
- Rewrite generated RTP to the local track's negotiated SSRC. Preserve MID
  extensions and valid VP8 Picture ID descriptors for forwarding.
- Wait for peer `Connected` before publishing: `write_rtp` queues packets and
  does not wait for the handshake or confirm network egress.
- Return promptly from `on_track`; run cancellable `TrackRemote::poll()` readers
  separately. Only `OnRtpPacket` counts as received media. Close readers, feedback
  pollers, and the peer driver with the transport.
- Bind loopback candidates to same-family loopback UDP addresses; other
  candidates use a matching-family wildcard bind.

The measurement window is shared across clients and excludes ramp/warmup. Keep
room admission separate from ICE/DTLS readiness and queued packets separate from
received RTP. Consumer buckets validate delivery within publisher lifetimes and
subscription caps.

## Debugging

Start with a small run using `--diagnostics`; inspect lifecycle events, resume
acknowledgments, transport states, and SSRC mappings as described under
[diagnosing missing media](../README.md#diagnosing-missing-media).
Use `RUST_LOG=error,rtc::peer_connection::handler=warn` for receive-path warnings;
broader debug logs may expose negotiation credentials.

The focused test suite includes a bounded two-peer loopback test that checks new
RTP across incremental consumer renegotiation and unchanged-mapping replay.
It exercises receiver registration and cleanup, not browser decoding or
mediasoup interoperability. Run it through the
[development command](../README.md#development).
