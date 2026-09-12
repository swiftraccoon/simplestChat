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
| `measurement.rs` | Shared interval, per-attempt readiness/coverage, consumer delivery checks |
| `subscriptions.rs` | Bounded discovery deduplication, pending queues, and consumer-cap refill |
| `receiver_stall.rs` | Opt-in bucket monitoring, shared capture budget, and scoped native capture |
| `keyframe_tests.rs` | Native startup/stall controls and bounded owned cleanup (tests only) |
| `mod.rs` | Module exports |

## How It Works

Each session creates separate send/receive peers, extracts local DTLS parameters
from an offer, and synthesizes remote SDP from the server's ICE/DTLS parameters.
Consumers are recorded in batches, installed through receive renegotiation, then
resumed through signaling.

Existing-room and live producer discovery feed the same audio/video FIFO queues.
The first dispatch tick is immediate; later ticks inspect at most two queued
items in total every 100 ms. Retired-item checks count toward that budget.
Skip a producer known to have ended through the owned generator lifecycle before
sending `Consume`; retain an already requested slot until the server sends
`ProducerClosed`. That notification frees capacity for queued discovery.
Discovery/deduplication is bounded to 20,000 identities and fails loudly at the
limit. Do not silently drop excess identities or bypass the queue on live events.
Keep pending consumer metadata outside the native peer until its live SDP batch;
a close before installation must cancel that work. Already-installed mappings
are retained, so consumer caps do not bound cumulative local transceiver count.

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
- Advertise supported video NACK, PLI and FIR consistently in producer parameters
  and SDP. Observe keyframe feedback before the default interceptor chain consumes
  RTCP, then preserve that chain's processing. Only requests targeting the owned
  video SSRC can set the single pending bit; repeated latest FIR identities
  (sender and sequence) are ignored without an unbounded sender registry.
- Satisfy feedback on a scheduled video frame, coalescing requests through a
  cooldown of `video_fps` frames after the preceding keyframe. Periodic keyframes
  retain their five-second frame phase and also satisfy pending requests. Extra
  keyframes increase synthetic traffic; comparisons require the same generator.
- Return promptly from `on_track`; run cancellable `TrackRemote::poll()` readers
  separately. Only `OnRtpPacket` counts as received media. Close readers and the
  peer driver with the transport; feedback observation adds no polling task.
- Bind loopback candidates to same-family loopback UDP addresses; other
  candidates use a matching-family wildcard bind.

The measurement window is shared across clients and excludes ramp/warmup. Keep
room admission separate from ICE/DTLS readiness and queued packets separate from
received RTP. Consumer buckets validate delivery within publisher lifetimes and
subscription caps.

Attempt coverage has a separate, preplanned eligibility interval:

- Freeze the deadline and stable-publisher expectations before WebSocket setup.
  Exclude three settling seconds and partial measurement seconds; never derive
  attempt eligibility from successful setup or consumer creation. Failed setup
  must not become a skipped short tail.
- Attribute consumers and queued packets to an immutable one-based attempt.
  Late callbacks must not acquire the next attempt's identity, and session end
  freezes queued counts before cleanup. Evidence after the planned deadline
  cannot rescue an attempt whose cleanup ran late. Count distinct stable
  publisher clients separately by media kind, not producer generations or
  duplicate consumers.
- Preserve existing gap checks for all created consumers. The additional
  `stable-publishers` floor does not prove complete dynamic fan-out, and dynamic
  consumers can occupy the configured caps. Keep that limitation visible.
- Keep additive report fields optional when reading historical JSON. Missing
  coverage is unavailable, not a passing or zero-expectation attempt.

Receiver-stall monitoring runs inside the session's existing wait-to-deadline,
not a detached task. Inspect existing buckets once per completed second, sharing
the delivery eligibility calculation; do not add packet-path work or routine
native polling. Share one admission budget across clients and reconnects. Dropping
a capture must release its native future, session lock and permit, and record
missing evidence on the original attempt. Keep stall entries outside pre-close
capacity and retain the original delivery failure after recovery. See the
[capture bounds and report fields](../README.md#capture-a-stalled-receiver).

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

A paired native SFU fixture also compares equal receiver observations with and
without advertised producer feedback. It checks delayed video startup against
observed keyframe generation and actual RTP receipt, then verifies ongoing
delivery. It is not a browser decoder or production-capacity test.

A paused-consumer case keeps native publisher ingress active, checks a single
sanitized receiver-stall capture, resumes actual RTP, and verifies that recovery
does not erase the gap or displace pre-close evidence. Both fixtures own and close
their synthetic loopback peers and workers within bounded cleanup.
