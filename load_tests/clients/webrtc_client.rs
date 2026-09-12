// Real ICE/DTLS/RTP client using the webrtc-rs 0.20 async Sans-I/O driver.
// Mediasoup's parameter-based signaling is adapted to the peer's SDP API.

use anyhow::{Context, Result};
use mediasoup::prelude::*;
use mediasoup_types::data_structures::{DtlsFingerprint, DtlsRole, IceCandidateType};
use rtc::interceptor::{Interceptor, Packet, StreamInfo, TaggedPacket, interceptor};
use rtc::rtcp::payload_feedbacks::{
    full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
};
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RTCRtpHeaderExtensionCapability, RtpCodecKind,
};
use rtc::sansio;
use rtc::shared::error::Error;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use webrtc::media_stream::MediaStreamTrack;
use webrtc::media_stream::track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceConnectionState, RTCPeerConnectionState,
    RTCSessionDescription, RTCStatsReportEntry, Registry, StatsSelector,
    register_default_interceptors,
};
use webrtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};

/// Observe requests before the default chain consumes RTCP. The original packet
/// still reaches every interceptor; this observer neither clones nor queues it.
#[derive(Interceptor)]
struct VideoFeedbackObserver<P> {
    #[next]
    next: P,
    video_ssrc: Option<u32>,
    requests: Arc<super::media_generator::KeyframeRequests>,
    cancellation: tokio::sync::watch::Receiver<bool>,
    metrics: Option<Arc<super::metrics::MetricsCollector>>,
    diagnostic_attempt: usize,
    // One SFU owns this transport. Retain only its latest FIR identity rather
    // than allocating a map proportional to arbitrary RTCP sender identities.
    last_fir: Option<(u32, u8)>,
}

impl<P> VideoFeedbackObserver<P> {
    fn request(&self, ssrc: u32, feedback: &str) {
        if self.requests.request()
            && let Some(metrics) = &self.metrics
            && metrics.diagnostics_enabled()
        {
            metrics.diagnostic_event_for_attempt(
                self.diagnostic_attempt,
                "keyframe-requested",
                serde_json::json!({"ssrc": ssrc, "feedback": feedback}),
            );
        }
    }

    fn observe(&mut self, packets: &[Box<dyn rtc::rtcp::Packet>]) {
        let Some(ssrc) = self.video_ssrc else {
            return;
        };
        if *self.cancellation.borrow() {
            return;
        }
        for packet in packets {
            if let Some(pli) = packet.as_any().downcast_ref::<PictureLossIndication>() {
                if pli.media_ssrc == ssrc {
                    self.request(ssrc, "pli");
                }
            } else if let Some(fir) = packet.as_any().downcast_ref::<FullIntraRequest>() {
                for entry in &fir.fir {
                    let identity = (fir.sender_ssrc, entry.sequence_number);
                    if entry.ssrc == ssrc && self.last_fir != Some(identity) {
                        self.last_fir = Some(identity);
                        self.request(ssrc, "fir");
                    }
                }
            }
        }
    }
}

#[interceptor]
impl<P: Interceptor> VideoFeedbackObserver<P> {
    #[overrides]
    fn handle_read(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message {
            self.observe(packets);
        }
        self.next.handle_read(msg)
    }

    #[overrides]
    fn close(&mut self) -> Result<(), Self::Error> {
        self.video_ssrc = None;
        self.next.close()
    }
}

struct TransportEvents {
    client_id: String,
    transport_id: String,
    metrics: Option<Arc<super::metrics::MetricsCollector>>,
    cancellation: tokio::sync::watch::Receiver<bool>,
    connection_state: tokio::sync::watch::Sender<RTCPeerConnectionState>,
    is_send: bool,
    diagnostic_attempt: usize,
}

async fn wait_for_connected(
    mut state: tokio::sync::watch::Receiver<RTCPeerConnectionState>,
    timeout: std::time::Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            match *state.borrow_and_update() {
                RTCPeerConnectionState::Connected => return Ok(()),
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    anyhow::bail!("Media transport failed or closed before connecting");
                }
                _ => {}
            }
            state
                .changed()
                .await
                .context("Media transport state channel closed")?;
        }
    })
    .await
    .context("Timed out waiting for media ICE/DTLS connection")?
}

async fn next_track_event<T>(
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    event: impl std::future::Future<Output = Option<T>>,
) -> Option<T> {
    if *cancellation.borrow() {
        return None;
    }
    tokio::select! {
        biased;
        _ = cancellation.changed() => None,
        value = event => value,
    }
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for TransportEvents {
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connection_state.send_replace(state);
        if let Some(metrics) = &self.metrics {
            if metrics.diagnostics_enabled() {
                metrics.diagnostic_event_for_attempt(
                    self.diagnostic_attempt,
                    "peer-state",
                    serde_json::json!({
                        "transportId": self.transport_id,
                        "direction": if self.is_send { "send" } else { "receive" },
                        "state": state.to_string(),
                        "intentionalClose": *self.cancellation.borrow(),
                    }),
                );
            }
            if state == RTCPeerConnectionState::Connected {
                metrics.mark_media_ready_for_attempt(self.diagnostic_attempt, self.is_send);
            }
            if state == RTCPeerConnectionState::Failed && !*self.cancellation.borrow() {
                metrics.record_error_for_attempt(
                    self.diagnostic_attempt,
                    format!("Media transport {} failed", self.transport_id),
                );
            }
        }
        match state {
            RTCPeerConnectionState::Connected => info!(
                "{}: Transport {} connected",
                self.client_id, self.transport_id
            ),
            RTCPeerConnectionState::Disconnected => warn!(
                "{}: Transport {} disconnected",
                self.client_id, self.transport_id
            ),
            RTCPeerConnectionState::Failed => {
                error!("{}: Transport {} failed", self.client_id, self.transport_id)
            }
            RTCPeerConnectionState::Closed => {
                debug!("{}: Transport {} closed", self.client_id, self.transport_id)
            }
            _ => {}
        }
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        if let Some(metrics) = &self.metrics
            && metrics.diagnostics_enabled()
        {
            metrics.diagnostic_event_for_attempt(
                self.diagnostic_attempt,
                "ice-state",
                serde_json::json!({
                    "transportId": self.transport_id,
                    "state": state.to_string(),
                }),
            );
        }
        debug!(
            "{}: Transport {} ICE state: {:?}",
            self.client_id, self.transport_id, state
        );
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let client_id = self.client_id.clone();
        let metrics = self.metrics.clone();
        let transport_id = self.transport_id.clone();
        let diagnostic_attempt = self.diagnostic_attempt;
        if let Some(metrics) = &metrics
            && metrics.diagnostics_enabled()
        {
            metrics.diagnostic_event_for_attempt(
                diagnostic_attempt,
                "track-callback",
                serde_json::json!({"transportId": transport_id}),
            );
        }
        let mut cancellation = self.cancellation.clone();
        // Return promptly: event dispatch must not wait for a track's lifetime.
        tokio::spawn(async move {
            let mut count = 0_u64;
            while let Some(event) = next_track_event(&mut cancellation, track.poll()).await {
                match event {
                    TrackRemoteEvent::OnRtpPacket(packet) => {
                        count += 1;
                        if let Some(metrics) = &metrics {
                            if count == 1 {
                                metrics.mark_first_media_received();
                                if metrics.diagnostics_enabled() {
                                    metrics.diagnostic_event_for_attempt(
                                        diagnostic_attempt,
                                        "track-first-rtp",
                                        serde_json::json!({
                                            "transportId": transport_id,
                                            "ssrc": packet.header.ssrc,
                                            "payloadType": packet.header.payload_type,
                                        }),
                                    );
                                }
                            }
                            metrics.record_rtp_received_for_attempt(
                                diagnostic_attempt,
                                packet.header.ssrc,
                                packet.payload.len(),
                            );
                        }
                        if count.is_multiple_of(500) {
                            debug!(
                                "{}: Received {} RTP packets (ssrc={})",
                                client_id, count, packet.header.ssrc
                            );
                        }
                    }
                    TrackRemoteEvent::OnError => {
                        if !*cancellation.borrow()
                            && let Some(metrics) = &metrics
                        {
                            metrics.record_error_for_attempt(
                                diagnostic_attempt,
                                "Remote RTP track error".into(),
                            );
                        }
                        break;
                    }
                    TrackRemoteEvent::OnEnded => break,
                    _ => {}
                }
            }
            debug!(
                "{}: Remote track stopped after {} packets",
                client_id, count
            );
        });
    }
}

/// Owns one mediasoup send/receive transport and its async WebRTC peer.
pub struct WebRtcTransport {
    peer_connection: Arc<dyn PeerConnection>,
    transport_id: String,
    client_id: String,
    is_send: bool,
    ice_candidate_inits: Vec<RTCIceCandidateInit>,
    ice_parameters: IceParameters,
    dtls_parameters: DtlsParameters,
    consumers: Vec<ConsumerInfo>,
    recv_transceiver_count: usize,
    cancellation: tokio::sync::watch::Sender<bool>,
    connection_state: tokio::sync::watch::Receiver<RTCPeerConnectionState>,
    send_audio_track: Option<Arc<TrackLocalStaticRTP>>,
    send_video_track: Option<Arc<TrackLocalStaticRTP>>,
    video_keyframe_requests: Arc<super::media_generator::KeyframeRequests>,
}

impl Drop for WebRtcTransport {
    fn drop(&mut self) {
        if !self.cancellation.send_replace(true) {
            // The default 0.20 peer driver does not close when its handle drops.
            // Setup errors or a cancelled client task must still release it.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let peer = self.peer_connection.clone();
                runtime.spawn(async move {
                    let _ = peer.close().await;
                });
            }
        }
    }
}

#[derive(Clone)]
struct ConsumerInfo {
    kind: MediaKind,
    ssrc: u32,
    mid_ext_id: Option<u16>,
    mid: usize,
    server_mid: Option<u32>,
}

fn audio_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: "audio/opus".to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

fn video_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: "video/VP8".to_owned(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line: String::new(),
        // Default interceptors provide NACK retransmission. The bounded video
        // observer additionally turns PLI/FIR into scheduled generator work.
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "nack".into(),
                parameter: String::new(),
            },
            RTCPFeedback {
                typ: "nack".into(),
                parameter: "pli".into(),
            },
            RTCPFeedback {
                typ: "ccm".into(),
                parameter: "fir".into(),
            },
        ],
    }
}

async fn attach_local_track(
    peer: &Arc<dyn PeerConnection>,
    client_id: &str,
    kind: RtpCodecKind,
    codec: RTCRtpCodec,
    ssrc: u32,
) -> Result<Arc<TrackLocalStaticRTP>> {
    let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("stream-{client_id}"),
        format!("{kind}-{client_id}"),
        format!("{kind} load test"),
        kind,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec,
            ..Default::default()
        }],
    )));
    peer.add_track(track.clone() as Arc<dyn TrackLocal>)
        .await
        .context("Failed to add local RTP track")?;
    Ok(track)
}

/// Bind loopback explicitly for local tests; wildcard expansion excludes it in 0.20.
fn local_udp_addresses(candidates: &[IceCandidate]) -> Vec<&'static str> {
    let addresses: Vec<std::net::IpAddr> = candidates
        .iter()
        .filter_map(|candidate| candidate.address.parse().ok())
        .collect();
    if !addresses.is_empty() && addresses.iter().all(std::net::IpAddr::is_loopback) {
        let mut result = Vec::new();
        if addresses.iter().any(std::net::IpAddr::is_ipv4) {
            result.push("127.0.0.1:0");
        }
        if addresses.iter().any(std::net::IpAddr::is_ipv6) {
            result.push("[::1]:0");
        }
        result
    } else {
        let mut result = vec!["0.0.0.0:0"];
        if addresses.iter().any(std::net::IpAddr::is_ipv6) {
            result.push("[::]:0");
        }
        result
    }
}

impl WebRtcTransport {
    pub async fn new(
        client_id: String,
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
        is_send: bool,
        metrics: Option<Arc<super::metrics::MetricsCollector>>,
    ) -> Result<(Self, DtlsParameters)> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: audio_codec(),
                payload_type: 111,
            },
            RtpCodecKind::Audio,
        )?;
        // The default registry appends NACK and PLI capabilities; register only
        // FIR here so the offer does not advertise duplicate feedback entries.
        let mut registered_video_codec = video_codec();
        registered_video_codec
            .rtcp_feedback
            .retain(|feedback| feedback.typ != "nack");
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: registered_video_codec,
                payload_type: 96,
            },
            RtpCodecKind::Video,
        )?;
        for kind in [RtpCodecKind::Audio, RtpCodecKind::Video] {
            media_engine.register_header_extension(
                RTCRtpHeaderExtensionCapability {
                    uri: "urn:ietf:params:rtp-hdrext:sdes:mid".to_owned(),
                },
                kind,
                None,
            )?;
        }
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
        let (cancellation, cancellation_rx) = tokio::sync::watch::channel(false);
        let video_ssrc = rand::random::<u32>();
        let video_keyframe_requests = Arc::new(super::media_generator::KeyframeRequests::default());
        let diagnostic_attempt = metrics
            .as_ref()
            .map(|m| m.diagnostic_attempt())
            .unwrap_or(0);
        let registry = registry.with(|next| VideoFeedbackObserver {
            next,
            video_ssrc: is_send.then_some(video_ssrc),
            requests: video_keyframe_requests.clone(),
            cancellation: cancellation.subscribe(),
            metrics: metrics.clone(),
            diagnostic_attempt,
            last_fir: None,
        });
        let (connection_state_tx, connection_state) =
            tokio::sync::watch::channel(RTCPeerConnectionState::New);
        let handler = Arc::new(TransportEvents {
            client_id: client_id.clone(),
            transport_id: transport_id.clone(),
            diagnostic_attempt,
            metrics,
            cancellation: cancellation_rx,
            connection_state: connection_state_tx,
            is_send,
        });
        let peer_connection: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(RTCConfigurationBuilder::default().build())
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .with_handler(handler)
                .with_udp_addrs(local_udp_addresses(&ice_candidates))
                .build()
                .await
                .context("Failed to create peer connection")?,
        );

        let pending_ice_candidates = ice_candidates
            .iter()
            .map(|candidate| {
                let protocol = match candidate.protocol {
                    Protocol::Udp => "udp",
                    Protocol::Tcp => "tcp",
                };
                let candidate_type = match candidate.r#type {
                    IceCandidateType::Host => "host",
                    IceCandidateType::Srflx => "srflx",
                    IceCandidateType::Prflx => "prflx",
                    IceCandidateType::Relay => "relay",
                };
                let tcp_type = if candidate.tcp_type.is_some() {
                    " tcptype passive"
                } else {
                    ""
                };
                RTCIceCandidateInit {
                    candidate: format!(
                        "candidate:{} 1 {} {} {} {} typ {}{}",
                        candidate.foundation,
                        protocol,
                        candidate.priority,
                        candidate.address,
                        candidate.port,
                        candidate_type,
                        tcp_type
                    ),
                    ..Default::default()
                }
            })
            .collect();

        let setup = async {
            let (send_audio_track, send_video_track) = if is_send {
                (
                    Some(
                        attach_local_track(
                            &peer_connection,
                            &client_id,
                            RtpCodecKind::Audio,
                            audio_codec(),
                            rand::random::<u32>(),
                        )
                        .await?,
                    ),
                    Some(
                        attach_local_track(
                            &peer_connection,
                            &client_id,
                            RtpCodecKind::Video,
                            video_codec(),
                            video_ssrc,
                        )
                        .await?,
                    ),
                )
            } else {
                for kind in [RtpCodecKind::Audio, RtpCodecKind::Video] {
                    peer_connection
                        .add_transceiver_from_kind(
                            kind,
                            Some(RTCRtpTransceiverInit {
                                direction: RTCRtpTransceiverDirection::Recvonly,
                                ..Default::default()
                            }),
                        )
                        .await
                        .context("Failed to add receive transceiver")?;
                }
                (None, None)
            };
            let transport = Self {
                peer_connection: peer_connection.clone(),
                transport_id,
                client_id,
                is_send,
                ice_candidate_inits: pending_ice_candidates,
                ice_parameters,
                dtls_parameters,
                consumers: Vec::new(),
                recv_transceiver_count: if is_send { 0 } else { 2 },
                cancellation: cancellation.clone(),
                connection_state,
                send_audio_track,
                send_video_track,
                video_keyframe_requests,
            };
            let local_dtls = transport.generate_dtls_parameters().await?;
            Ok::<_, anyhow::Error>((transport, local_dtls))
        }
        .await;
        if setup.is_err() && !cancellation.send_replace(true) {
            let _ = peer_connection.close().await;
        }
        setup
    }

    /// Generate local DTLS parameters from the peer connection
    async fn generate_dtls_parameters(&self) -> Result<DtlsParameters> {
        let offer = self
            .peer_connection
            .create_offer(None)
            .await
            .context("Failed to create offer")?;

        self.peer_connection
            .set_local_description(offer)
            .await
            .context("Failed to set local description")?;

        let local_desc = self
            .peer_connection
            .local_description()
            .await
            .context("No local description")?;

        let fingerprint = extract_fingerprint_from_sdp(&local_desc.sdp)?;

        // Client always takes DTLS client role (active side).
        let role = DtlsRole::Client;

        Ok(DtlsParameters {
            role,
            fingerprints: vec![fingerprint],
        })
    }

    /// Set remote SDP to complete the connection
    pub async fn set_remote_description(
        &mut self,
        ice_parameters: &IceParameters,
        dtls_parameters: &DtlsParameters,
    ) -> Result<()> {
        let remote_sdp = generate_remote_sdp(
            ice_parameters,
            dtls_parameters,
            self.is_send,
            &self.consumers,
        )?;

        let remote_desc = RTCSessionDescription::answer(remote_sdp)?;

        self.peer_connection
            .set_remote_description(remote_desc)
            .await
            .context("Failed to set remote description")?;

        debug!(
            "{}: Remote description set for transport {}",
            self.client_id, self.transport_id
        );

        // Now add ICE candidates (must be after remote description is set)
        for candidate in &self.ice_candidate_inits {
            self.peer_connection
                .add_ice_candidate(candidate.clone())
                .await
                .context("Failed to add ICE candidate")?;
        }

        debug!(
            "{}: ICE candidates added for transport {}",
            self.client_id, self.transport_id
        );

        Ok(())
    }

    /// Record a consumer for later SDP renegotiation (does NOT renegotiate yet).
    ///
    /// Call `renegotiate_consumers()` after all consumers are recorded to do a
    /// single SDP renegotiation that registers ALL SSRCs at once. This avoids
    /// a webrtc-rs bug where the second `set_remote_description` during rapid
    /// renegotiation doesn't properly register new SSRCs.
    pub fn add_consumer_info(&mut self, kind: MediaKind, consumer_rtp_parameters: &RtpParameters) {
        let ssrc = consumer_rtp_parameters
            .encodings
            .first()
            .and_then(|e| e.ssrc)
            .unwrap_or(0);

        let mid_ext_id = consumer_rtp_parameters
            .header_extensions
            .iter()
            .find(|ext| ext.uri == RtpHeaderExtensionUri::Mid)
            .map(|ext| ext.id);

        debug!(
            "{}: Recording consumer: kind={:?}, ssrc={}, mid_ext_id={:?}, consumer_mid={:?}",
            self.client_id, kind, ssrc, mid_ext_id, consumer_rtp_parameters.mid,
        );

        if self
            .consumers
            .iter()
            .any(|consumer| consumer.ssrc == ssrc && consumer.kind == kind)
        {
            return;
        }
        // A 0.20 receiver owns one track, so every producer needs its own
        // transceiver/m-line. Reuse the initial audio and video negotiation slots.
        let mid = if !self.consumers.iter().any(|consumer| consumer.kind == kind) {
            match kind {
                MediaKind::Audio => 0,
                MediaKind::Video => 1,
            }
        } else {
            2 + self
                .consumers
                .iter()
                .filter(|consumer| consumer.mid >= 2)
                .count()
        };
        self.consumers.push(ConsumerInfo {
            kind,
            ssrc,
            mid_ext_id,
            mid,
            server_mid: consumer_rtp_parameters
                .mid
                .as_ref()
                .and_then(|mid| mid.parse().ok()),
        });
    }

    /// Renegotiate SDP with ALL recorded consumers at once.
    ///
    /// This ensures all consumer SSRCs are registered in a single
    /// `set_remote_description` call, avoiding issues with webrtc-rs
    /// not properly tracking SSRCs added in subsequent renegotiations.
    pub async fn renegotiate_consumers(&mut self) -> Result<()> {
        if self.consumers.is_empty() {
            return Ok(());
        }

        debug!(
            "{}: Renegotiating SDP for {} consumers",
            self.client_id,
            self.consumers.len()
        );

        let mut additional: Vec<&ConsumerInfo> = self
            .consumers
            .iter()
            .filter(|consumer| consumer.mid >= self.recv_transceiver_count)
            .collect();
        additional.sort_by_key(|consumer| consumer.mid);
        for consumer in additional {
            let kind = match consumer.kind {
                MediaKind::Audio => RtpCodecKind::Audio,
                MediaKind::Video => RtpCodecKind::Video,
            };
            self.peer_connection
                .add_transceiver_from_kind(
                    kind,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        ..Default::default()
                    }),
                )
                .await
                .context("Failed to add consumer receive transceiver")?;
            self.recv_transceiver_count += 1;
        }

        let offer = self
            .peer_connection
            .create_offer(None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create renegotiation offer: {}", e))?;

        self.peer_connection
            .set_local_description(offer)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to set local desc for renegotiation: {}", e))?;

        let remote_sdp = generate_remote_sdp(
            &self.ice_parameters,
            &self.dtls_parameters,
            self.is_send,
            &self.consumers,
        )?;

        debug!(
            "{}: Renegotiation answer SDP:\n{}",
            self.client_id, remote_sdp
        );

        let remote_desc = RTCSessionDescription::answer(remote_sdp)?;

        self.peer_connection
            .set_remote_description(remote_desc)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to set remote desc for renegotiation: {}", e))?;

        info!(
            "{}: SDP renegotiated for {} consumers (SSRCs: {:?})",
            self.client_id,
            self.consumers.len(),
            self.consumers.iter().map(|c| c.ssrc).collect::<Vec<_>>()
        );

        Ok(())
    }

    /// Lifetime RTC counters are diagnostic evidence, not steady-window metrics.
    /// The caller bounds this entire operation, including peer/session locks.
    async fn diagnostic_snapshot(&self) -> Result<serde_json::Value> {
        let report = self
            .peer_connection
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await;
        let mut stats = Vec::new();
        let mut has_transport_stats = false;
        for entry in report.iter() {
            let value = match entry {
                RTCStatsReportEntry::Transport(s) => {
                    has_transport_stats = true;
                    serde_json::to_value(s)?
                }
                RTCStatsReportEntry::IceCandidatePair(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::LocalCandidate(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::RemoteCandidate(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::InboundRtp(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::OutboundRtp(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::RemoteInboundRtp(s) => serde_json::to_value(s)?,
                RTCStatsReportEntry::RemoteOutboundRtp(s) => serde_json::to_value(s)?,
                // Certificates are never serialized. Candidate addresses and
                // credentials are removed by the explicit field allowlist.
                _ => continue,
            };
            anyhow::ensure!(stats.len() < 512, "RTC diagnostic stats limit exceeded");
            stats.push(sanitize_rtc_stat(&value));
        }
        anyhow::ensure!(
            has_transport_stats,
            "RTC diagnostic report omitted transport stats"
        );
        stats.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let local = self
            .peer_connection
            .local_description()
            .await
            .context("Diagnostic local SDP missing")?;
        let remote = self
            .peer_connection
            .remote_description()
            .await
            .context("Diagnostic remote SDP missing")?;
        let local_media = sanitize_sdp_media(&local.sdp);
        let remote_media = sanitize_sdp_media(&remote.sdp);
        anyhow::ensure!(
            local_media.len() <= 256 && remote_media.len() <= 256 && self.consumers.len() <= 256,
            "SDP diagnostic mapping limit exceeded"
        );
        Ok(serde_json::json!({
            "transportId": self.transport_id,
            "direction": if self.is_send { "send" } else { "receive" },
            "connectionState": self.connection_state.borrow().to_string(),
            "statsSource": "webrtc-rs-0.20/get_stats; lifetime, not measurement-window counters",
            "stats": stats,
            "localMedia": local_media,
            "remoteMedia": remote_media,
            "consumerMappings": self.consumers.iter().map(|c| serde_json::json!({
                "kind": c.kind,
                "ssrc": c.ssrc,
                "localMid": c.mid,
                "serverMid": c.server_mid,
                "midExtensionId": c.mid_ext_id,
            })).collect::<Vec<_>>(),
        }))
    }

    /// Get the SSRCs assigned by webrtc-rs for send tracks from the local SDP
    pub async fn get_send_ssrcs(&self) -> Result<(u32, u32)> {
        let local_desc = self
            .peer_connection
            .local_description()
            .await
            .context("No local description for send transport")?;

        let mut audio_ssrc = None;
        let mut video_ssrc = None;
        let mut in_audio = false;
        let mut in_video = false;

        for line in local_desc.sdp.lines() {
            if line.starts_with("m=audio") {
                in_audio = true;
                in_video = false;
            } else if line.starts_with("m=video") {
                in_audio = false;
                in_video = true;
            } else if let Some(ssrc_str) = line.strip_prefix("a=ssrc:")
                && let Some(ssrc_num_str) = ssrc_str.split_whitespace().next()
                && let Ok(ssrc) = ssrc_num_str.parse::<u32>()
            {
                if in_audio && audio_ssrc.is_none() {
                    audio_ssrc = Some(ssrc);
                } else if in_video && video_ssrc.is_none() {
                    video_ssrc = Some(ssrc);
                }
            }
        }

        Ok((
            audio_ssrc.context("No audio SSRC found in local SDP")?,
            video_ssrc.context("No video SSRC found in local SDP")?,
        ))
    }

    /// Close the transport
    pub async fn close(&self) -> Result<()> {
        if self.cancellation.send_replace(true) {
            return Ok(());
        }
        self.peer_connection
            .close()
            .await
            .context("Failed to close peer connection")?;
        info!("{}: Transport {} closed", self.client_id, self.transport_id);
        Ok(())
    }
}

/// Complete WebRTC client session managing both send and receive transports
pub struct WebRtcSession {
    client_id: String,
    send_transport: Option<WebRtcTransport>,
    recv_transport: Option<WebRtcTransport>,
    audio_track: Option<Arc<TrackLocalStaticRTP>>,
    video_track: Option<Arc<TrackLocalStaticRTP>>,
    /// Metrics collector passed to recv transport's on_track handler
    metrics: Option<Arc<super::metrics::MetricsCollector>>,
}

impl WebRtcSession {
    pub fn new(client_id: String, metrics: Arc<super::metrics::MetricsCollector>) -> Self {
        Self {
            client_id,
            send_transport: None,
            recv_transport: None,
            audio_track: None,
            video_track: None,
            metrics: Some(metrics),
        }
    }

    /// Create send transport (tracks are added before SDP negotiation)
    pub async fn create_send_transport(
        &mut self,
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
    ) -> Result<DtlsParameters> {
        let (mut transport, local_dtls) = WebRtcTransport::new(
            self.client_id.clone(),
            transport_id,
            ice_parameters.clone(),
            ice_candidates.clone(),
            dtls_parameters.clone(),
            true,
            self.metrics.clone(),
        )
        .await?;

        if let Err(error) = transport
            .set_remote_description(&ice_parameters, &dtls_parameters)
            .await
        {
            let _ = transport.close().await;
            return Err(error);
        }

        // Store tracks created by send transport (added before SDP negotiation)
        self.audio_track = transport.send_audio_track.clone();
        self.video_track = transport.send_video_track.clone();

        self.send_transport = Some(transport);
        Ok(local_dtls)
    }

    /// Create receive transport
    pub async fn create_recv_transport(
        &mut self,
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
    ) -> Result<DtlsParameters> {
        let (mut transport, local_dtls) = WebRtcTransport::new(
            self.client_id.clone(),
            transport_id,
            ice_parameters.clone(),
            ice_candidates.clone(),
            dtls_parameters.clone(),
            false,
            self.metrics.clone(), // on_track handler increments metrics directly
        )
        .await?;

        if let Err(error) = transport
            .set_remote_description(&ice_parameters, &dtls_parameters)
            .await
        {
            let _ = transport.close().await;
            return Err(error);
        }

        self.recv_transport = Some(transport);
        Ok(local_dtls)
    }

    /// Get the actual SSRCs assigned by webrtc-rs for the send transport
    pub async fn send_ssrcs(&self) -> Result<(u32, u32)> {
        let transport = self.send_transport.as_ref().context("No send transport")?;
        transport.get_send_ssrcs().await
    }

    /// The async writer queues packets; it does not wait for ICE/DTLS itself.
    /// Gate generation so initial media/keyframes aren't dropped before SRTP exists.
    pub async fn wait_send_connected(&self) -> Result<()> {
        let transport = self.send_transport.as_ref().context("No send transport")?;
        wait_for_connected(
            transport.connection_state.clone(),
            std::time::Duration::from_secs(10),
        )
        .await
    }

    /// Record a consumer for later batched SDP renegotiation
    pub fn record_consumer(
        &mut self,
        _producer_id: String,
        kind: MediaKind,
        consumer_rtp_parameters: &RtpParameters,
    ) -> Result<()> {
        let transport = self
            .recv_transport
            .as_mut()
            .context("Receive transport not created")?;

        transport.add_consumer_info(kind, consumer_rtp_parameters);
        Ok(())
    }

    /// Renegotiate SDP for all recorded consumers at once
    pub async fn renegotiate_consumers(&mut self) -> Result<()> {
        let transport = self
            .recv_transport
            .as_mut()
            .context("Receive transport not created")?;

        transport.renegotiate_consumers().await
    }

    pub fn audio_track(&self) -> Option<Arc<TrackLocalStaticRTP>> {
        self.audio_track.clone()
    }

    pub fn video_track(&self) -> Option<Arc<TrackLocalStaticRTP>> {
        self.video_track.clone()
    }

    /// Return this send transport's bounded request latch, never a prior session's.
    pub fn video_keyframe_requests(&self) -> Option<Arc<super::media_generator::KeyframeRequests>> {
        self.send_transport
            .as_ref()
            .map(|transport| transport.video_keyframe_requests.clone())
    }

    pub async fn diagnostic_snapshot(&self) -> Result<serde_json::Value> {
        let mut transports = Vec::new();
        if let Some(transport) = &self.send_transport {
            transports.push(transport.diagnostic_snapshot().await?);
        }
        if let Some(transport) = &self.recv_transport {
            transports.push(transport.diagnostic_snapshot().await?);
        }
        anyhow::ensure!(
            !transports.is_empty(),
            "No transports for diagnostic snapshot"
        );
        Ok(serde_json::json!({"transports": transports}))
    }

    /// Close all transports
    pub async fn close(&self) -> Result<()> {
        let mut result = Ok(());
        if let Some(t) = &self.send_transport {
            result = t.close().await;
        }
        if let Some(t) = &self.recv_transport {
            let closed = t.close().await;
            if result.is_ok() {
                result = closed;
            }
        }
        result
    }
}

/// Explicit allowlist, rather than deleting today's known credential fields.
/// Omit certificate entries at the call site as well. Candidate ports allow a
/// loopback packet capture to be joined to a peer without exposing IP addresses.
fn sanitize_rtc_stat(value: &serde_json::Value) -> serde_json::Value {
    const FIELDS: &[&str] = &[
        "id",
        "type",
        "timestamp",
        "transportId",
        "ssrc",
        "kind",
        "mid",
        "packetsSent",
        "packetsReceived",
        "bytesSent",
        "bytesReceived",
        "headerBytesSent",
        "headerBytesReceived",
        "packetsLost",
        "packetsDiscarded",
        "jitter",
        "lastPacketReceivedTimestamp",
        "lastPacketSentTimestamp",
        "iceRole",
        "iceState",
        "dtlsState",
        "dtlsRole",
        "selectedCandidatePairId",
        "selectedCandidatePairChanges",
        "state",
        "nominated",
        "localCandidateId",
        "remoteCandidateId",
        "currentRoundTripTime",
        "totalRoundTripTime",
        "roundTripTime",
        "requestsReceived",
        "requestsSent",
        "responsesReceived",
        "responsesSent",
        "consentRequestsSent",
        "packetsDiscardedOnSend",
        "bytesDiscardedOnSend",
        "nackCount",
        "pliCount",
        "firCount",
        "port",
        "protocol",
        "candidateType",
    ];
    let mut fields: serde_json::Map<String, serde_json::Value> = FIELDS
        .iter()
        .filter_map(|field| {
            value
                .get(*field)
                .map(|value| ((*field).to_string(), value.clone()))
        })
        .collect();
    if let Some(loopback) = value
        .get("address")
        .and_then(|address| address.as_str())
        .and_then(|address| address.parse::<std::net::IpAddr>().ok())
        .map(|address| address.is_loopback())
    {
        fields.insert("isLoopback".into(), loopback.into());
    }
    serde_json::Value::Object(fields)
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SdpMediaDiagnostic {
    kind: String,
    mid: Option<u32>,
    direction: Option<String>,
    payload_types: Vec<u8>,
    ssrcs: Vec<u32>,
}

/// Never retain raw SDP lines: only numeric mappings and fixed media labels.
fn sanitize_sdp_media(sdp: &str) -> Vec<SdpMediaDiagnostic> {
    let mut media: Vec<SdpMediaDiagnostic> = Vec::new();
    for line in sdp.lines() {
        if let Some(mline) = line.strip_prefix("m=") {
            let fields: Vec<_> = mline.split_whitespace().collect();
            media.push(SdpMediaDiagnostic {
                kind: match fields.first().copied() {
                    Some("audio") => "audio",
                    Some("video") => "video",
                    _ => "other",
                }
                .to_string(),
                payload_types: fields
                    .iter()
                    .skip(3)
                    .filter_map(|pt| pt.parse::<u8>().ok())
                    .collect(),
                ..Default::default()
            });
        } else if let Some(section) = media.last_mut() {
            if let Some(mid) = line.strip_prefix("a=mid:") {
                section.mid = mid.parse().ok();
            } else if matches!(
                line,
                "a=sendonly" | "a=recvonly" | "a=sendrecv" | "a=inactive"
            ) {
                section.direction = Some(line[2..].to_string());
            } else if let Some(ssrc) = line
                .strip_prefix("a=ssrc:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok())
                && !section.ssrcs.contains(&ssrc)
            {
                section.ssrcs.push(ssrc);
            }
        }
    }
    media
}

/// Extract DTLS fingerprint from SDP
fn extract_fingerprint_from_sdp(sdp: &str) -> Result<DtlsFingerprint> {
    for line in sdp.lines() {
        if line.starts_with("a=fingerprint:") {
            let parts: Vec<&str> = line
                .trim_start_matches("a=fingerprint:")
                .splitn(2, ' ')
                .collect();
            if parts.len() == 2 {
                let algorithm = parts[0];
                let value_str = parts[1];

                let bytes: Vec<u8> = value_str
                    .split(':')
                    .filter_map(|hex| u8::from_str_radix(hex, 16).ok())
                    .collect();

                return match (algorithm, bytes.len()) {
                    ("sha-1", 20) => {
                        let mut value = [0u8; 20];
                        value.copy_from_slice(&bytes);
                        Ok(DtlsFingerprint::Sha1 { value })
                    }
                    ("sha-224", 28) => {
                        let mut value = [0u8; 28];
                        value.copy_from_slice(&bytes);
                        Ok(DtlsFingerprint::Sha224 { value })
                    }
                    ("sha-256", 32) => {
                        let mut value = [0u8; 32];
                        value.copy_from_slice(&bytes);
                        Ok(DtlsFingerprint::Sha256 { value })
                    }
                    ("sha-384", 48) => {
                        let mut value = [0u8; 48];
                        value.copy_from_slice(&bytes);
                        Ok(DtlsFingerprint::Sha384 { value })
                    }
                    ("sha-512", 64) => {
                        let mut value = [0u8; 64];
                        value.copy_from_slice(&bytes);
                        Ok(DtlsFingerprint::Sha512 { value })
                    }
                    _ => Err(anyhow::anyhow!(
                        "Unsupported fingerprint algorithm or length"
                    )),
                };
            }
        }
    }

    Err(anyhow::anyhow!("No fingerprint found in SDP"))
}

/// Generate a remote SDP answer with mediasoup ICE/DTLS parameters
///
/// For recv transports, includes ALL consumer SSRCs and mid extension for proper
/// RTP packet routing in webrtc-rs. For send transports, generates a simple answer.
fn generate_remote_sdp(
    ice_parameters: &IceParameters,
    dtls_parameters: &DtlsParameters,
    is_send: bool,
    consumers: &[ConsumerInfo],
) -> Result<String> {
    // webrtc-rs only supports SHA-256 fingerprints. mediasoup returns all
    // algorithms (SHA-1, SHA-224, SHA-256, SHA-384, SHA-512) in non-deterministic
    // order (from absl::flat_hash_map), so we must find SHA-256 specifically.
    let fingerprint = dtls_parameters
        .fingerprints
        .iter()
        .find(|fp| matches!(fp, DtlsFingerprint::Sha256 { .. }))
        .context("No SHA-256 DTLS fingerprint provided")?;

    let (fp_algorithm, fp_value) = match fingerprint {
        DtlsFingerprint::Sha1 { value } => ("sha-1", hex_encode(value)),
        DtlsFingerprint::Sha224 { value } => ("sha-224", hex_encode(value)),
        DtlsFingerprint::Sha256 { value } => ("sha-256", hex_encode(value)),
        DtlsFingerprint::Sha384 { value } => ("sha-384", hex_encode(value)),
        DtlsFingerprint::Sha512 { value } => ("sha-512", hex_encode(value)),
    };

    // DTLS setup: server is always passive, client is active
    let setup = match dtls_parameters.role {
        DtlsRole::Client => "active",
        DtlsRole::Server => "passive",
        DtlsRole::Auto => "passive",
    };

    // Each received producer owns an m-line; putting several independent
    // SSRCs in one section loses tracks in the 0.20 Sans-I/O receiver model.
    let mut sections = vec![(0, MediaKind::Audio), (1, MediaKind::Video)];
    sections.extend(
        consumers
            .iter()
            .filter(|consumer| consumer.mid >= 2)
            .map(|consumer| (consumer.mid, consumer.kind)),
    );
    sections.sort_by_key(|(mid, _)| *mid);
    let mids = sections
        .iter()
        .map(|(mid, _)| mid.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut sdp = format!(
        "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=group:BUNDLE {mids}\r\n\
         a=ice-ufrag:{}\r\na=ice-pwd:{}\r\n",
        ice_parameters.username_fragment, ice_parameters.password,
    );
    if ice_parameters.ice_lite == Some(true) {
        sdp.push_str("a=ice-lite\r\n");
    }
    for (mid, kind) in sections {
        let consumer = consumers.iter().find(|consumer| consumer.mid == mid);
        let (media, payload_type, codec) = match kind {
            MediaKind::Audio => ("audio", 111, "opus/48000/2"),
            MediaKind::Video => ("video", 96, "VP8/90000"),
        };
        let direction = if is_send { "recvonly" } else { "sendonly" };
        let extension = consumer
            .and_then(|consumer| consumer.mid_ext_id)
            .unwrap_or(1);
        sdp.push_str(&format!(
            "m={media} 9 UDP/TLS/RTP/SAVPF {payload_type}\r\n\
             c=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=rtcp-mux\r\n\
             a=mid:{mid}\r\na={direction}\r\na=rtpmap:{payload_type} {codec}\r\n\
             a=extmap:{extension} urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
             a=fingerprint:{fp_algorithm} {fp_value}\r\na=setup:{setup}\r\n",
        ));
        if kind == MediaKind::Audio {
            sdp.push_str("a=fmtp:111 minptime=10;useinbandfec=1\r\n");
        } else {
            sdp.push_str("a=rtcp-fb:96 nack\r\na=rtcp-fb:96 nack pli\r\na=rtcp-fb:96 ccm fir\r\n");
        }
        if let Some(consumer) = consumer {
            let ssrc = consumer.ssrc;
            sdp.push_str(&format!(
                "a=msid:mediasoup-{ssrc} consumer-{ssrc}\r\n\
                 a=ssrc:{ssrc} cname:mediasoup\r\na=ssrc:{ssrc} msid:mediasoup-{ssrc} consumer-{ssrc}\r\n"
            ));
        }
    }
    Ok(sdp)
}

/// Convert byte array to colon-separated hex string for SDP
fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use futures_util::FutureExt;
    use rtc::interceptor::NoopInterceptor;
    use rtc::rtcp::payload_feedbacks::full_intra_request::FirEntry;
    use rtc::sansio::Protocol as _;
    use std::time::Duration;
    use webrtc::media_stream::Track;

    #[derive(Interceptor)]
    struct FeedbackProbe<P> {
        #[next]
        next: P,
        reads: usize,
        last_read: Option<Packet>,
    }

    #[interceptor]
    impl<P: Interceptor> FeedbackProbe<P> {
        #[overrides]
        fn handle_read(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
            self.reads += 1;
            self.last_read = Some(msg.message.clone());
            self.next.handle_read(msg)
        }
    }

    struct FeedbackFixture {
        observer: VideoFeedbackObserver<FeedbackProbe<NoopInterceptor>>,
        cancellation: tokio::sync::watch::Sender<bool>,
    }

    impl FeedbackFixture {
        fn new() -> Self {
            let (cancellation, receiver) = tokio::sync::watch::channel(false);
            let metrics = Arc::new(super::super::metrics::MetricsCollector::new(
                "feedback-test".into(),
            ));
            metrics.begin_connection_attempt();
            metrics.enable_diagnostics();
            Self {
                observer: VideoFeedbackObserver {
                    next: FeedbackProbe {
                        next: NoopInterceptor::new(),
                        reads: 0,
                        last_read: None,
                    },
                    video_ssrc: Some(7),
                    requests: Arc::new(super::super::media_generator::KeyframeRequests::default()),
                    cancellation: receiver,
                    metrics: Some(metrics),
                    diagnostic_attempt: 1,
                    last_fir: None,
                },
                cancellation,
            }
        }

        fn receive(&mut self, packets: Vec<Box<dyn rtc::rtcp::Packet>>) {
            self.observer
                .handle_read(TaggedPacket {
                    now: std::time::Instant::now(),
                    transport: Default::default(),
                    message: Packet::Rtcp(packets),
                })
                .unwrap();
        }
    }

    fn pli(ssrc: u32) -> Box<dyn rtc::rtcp::Packet> {
        Box::new(PictureLossIndication {
            sender_ssrc: 99,
            media_ssrc: ssrc,
        })
    }

    fn fir(sender_ssrc: u32, ssrc: u32, sequence_number: u8) -> Box<dyn rtc::rtcp::Packet> {
        Box::new(FullIntraRequest {
            sender_ssrc,
            // FIR targets are carried by entries, not this field.
            media_ssrc: 0,
            fir: vec![FirEntry {
                ssrc,
                sequence_number,
            }],
        })
    }

    #[test]
    fn video_feedback_filters_targets_and_coalesces_attempt_scoped_diagnostics() {
        let mut fixture = FeedbackFixture::new();
        fixture.receive(vec![
            pli(8),
            fir(99, 8, 1),
            Box::new(rtc::rtcp::receiver_report::ReceiverReport::default()),
            Box::new(
                rtc::rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack {
                    media_ssrc: 7,
                    ..Default::default()
                },
            ),
        ]);
        assert!(!fixture.observer.requests.take());
        fixture
            .observer
            .metrics
            .as_ref()
            .unwrap()
            .begin_connection_attempt();
        fixture.receive(vec![pli(7), pli(7), pli(7)]);
        assert!(fixture.observer.requests.take());
        fixture.receive(vec![pli(7)]);
        assert!(fixture.observer.requests.take());
        let report = fixture.observer.metrics.as_ref().unwrap().generate_report();
        let events = &report.diagnostics.unwrap().events;
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.attempt == 1 && event.kind == "keyframe-requested")
        );
        assert_eq!(
            events[0].details,
            serde_json::json!({"ssrc": 7, "feedback": "pli"})
        );
    }

    #[test]
    fn video_feedback_fir_duplicates_are_bounded_and_sequence_wrap_is_valid() {
        let mut fixture = FeedbackFixture::new();
        fixture.receive(vec![fir(99, 7, 255)]);
        assert!(fixture.observer.requests.take());
        fixture.receive(vec![fir(99, 7, 255)]);
        assert!(!fixture.observer.requests.take());
        fixture.receive(vec![fir(99, 8, 0)]);
        assert_eq!(fixture.observer.last_fir, Some((99, 255)));
        fixture.receive(vec![fir(99, 7, 0)]);
        assert!(fixture.observer.requests.take());
        fixture.receive(vec![fir(100, 7, 0)]);
        assert!(fixture.observer.requests.take());
        assert_eq!(fixture.observer.last_fir, Some((100, 0)));
        let report = fixture.observer.metrics.as_ref().unwrap().generate_report();
        let events = &report.diagnostics.unwrap().events;
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.details["feedback"] == "fir")
        );
    }

    #[test]
    fn video_feedback_cancellation_close_and_receive_only_never_request_frames() {
        let mut fixture = FeedbackFixture::new();
        fixture.cancellation.send_replace(true);
        fixture.receive(vec![pli(7), fir(99, 7, 1)]);
        assert!(!fixture.observer.requests.take());
        assert_eq!(fixture.observer.next.reads, 1);

        let mut fixture = FeedbackFixture::new();
        fixture.observer.close().unwrap();
        fixture.receive(vec![pli(7), fir(99, 7, 1)]);
        assert!(!fixture.observer.requests.take());
        assert_eq!(fixture.observer.next.reads, 1);

        let mut fixture = FeedbackFixture::new();
        fixture.observer.video_ssrc = None;
        fixture.receive(vec![pli(7), fir(99, 7, 1)]);
        assert!(!fixture.observer.requests.take());
        assert_eq!(fixture.observer.next.reads, 1);
    }

    #[test]
    fn video_feedback_preserves_inner_packet_processing_and_rtp_passthrough() {
        let mut fixture = FeedbackFixture::new();
        let packets = vec![pli(7), fir(99, 7, 1)];
        let expected = Packet::Rtcp(packets.clone());
        fixture.receive(packets);
        assert_eq!(fixture.observer.next.last_read.as_ref(), Some(&expected));
        assert_eq!(fixture.observer.next.reads, 1);
        assert!(
            fixture.observer.poll_read().is_none(),
            "terminal consumes RTCP as before"
        );
        assert!(fixture.observer.requests.take());

        let rtp = rtc::rtp::Packet::default();
        fixture
            .observer
            .handle_read(TaggedPacket {
                now: std::time::Instant::now(),
                transport: Default::default(),
                message: Packet::Rtp(rtp.clone()),
            })
            .unwrap();
        assert_eq!(
            fixture.observer.poll_read().unwrap().message,
            Packet::Rtp(rtp.clone())
        );
        fixture
            .observer
            .handle_write(TaggedPacket {
                now: std::time::Instant::now(),
                transport: Default::default(),
                message: Packet::Rtp(rtp.clone()),
            })
            .unwrap();
        assert_eq!(
            fixture.observer.poll_write().unwrap().message,
            Packet::Rtp(rtp)
        );
        assert!(!fixture.observer.requests.take());
    }

    #[test]
    fn video_feedback_observer_precedes_the_complete_default_interceptor_chain() {
        let (cancellation, receiver) = tokio::sync::watch::channel(false);
        let requests = Arc::new(super::super::media_generator::KeyframeRequests::default());
        let mut media_engine = MediaEngine::default();
        let registry = register_default_interceptors(Registry::new(), &mut media_engine).unwrap();
        let mut observer = registry
            .with(|next| VideoFeedbackObserver {
                next,
                video_ssrc: Some(7),
                requests: requests.clone(),
                cancellation: receiver,
                metrics: None,
                diagnostic_attempt: 0,
                last_fir: None,
            })
            .build();
        observer
            .handle_read(TaggedPacket {
                now: std::time::Instant::now(),
                transport: Default::default(),
                message: Packet::Rtcp(vec![pli(7)]),
            })
            .unwrap();
        assert!(requests.take());
        assert!(observer.poll_read().is_none());
        cancellation.send_replace(true);
        observer.close().unwrap();
    }

    struct LoopbackSenderEvents {
        state: tokio::sync::watch::Sender<RTCPeerConnectionState>,
        gathered: tokio::sync::watch::Sender<bool>,
    }

    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for LoopbackSenderEvents {
        async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
            self.state.send_replace(state);
        }

        async fn on_ice_gathering_state_change(
            &self,
            state: webrtc::peer_connection::RTCIceGatheringState,
        ) {
            if state == webrtc::peer_connection::RTCIceGatheringState::Complete {
                self.gathered.send_replace(true);
            }
        }
    }

    #[derive(Clone)]
    struct LoopbackTrack {
        track: Arc<TrackLocalStaticRTP>,
        kind: MediaKind,
        ssrc: u32,
        queued: Arc<std::sync::atomic::AtomicU64>,
    }

    async fn add_loopback_track(
        peer: &Arc<dyn PeerConnection>,
        index: usize,
    ) -> Result<LoopbackTrack> {
        let (kind, codec_kind, codec) = if index.is_multiple_of(2) {
            (MediaKind::Audio, RtpCodecKind::Audio, audio_codec())
        } else {
            (MediaKind::Video, RtpCodecKind::Video, video_codec())
        };
        let ssrc = 10_000 + index as u32;
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            format!("loopback-stream-{index}"),
            format!("loopback-track-{index}"),
            "test RTP".into(),
            codec_kind,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec,
                ..Default::default()
            }],
        )));
        peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        anyhow::ensure!(track.ssrcs().await == vec![ssrc], "fixture SSRC changed");
        Ok(LoopbackTrack {
            track,
            kind,
            ssrc,
            queued: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    fn loopback_ice_parameters(sdp: &str, ice_lite: bool) -> Result<IceParameters> {
        let attribute = |prefix: &str| -> Result<String> {
            Ok(sdp
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .context("fixture SDP omitted ICE parameters")?
                .to_string())
        };
        Ok(IceParameters {
            username_fragment: attribute("a=ice-ufrag:")?,
            password: attribute("a=ice-pwd:")?,
            ice_lite: Some(ice_lite),
        })
    }

    fn loopback_packet_counts(
        metrics: &super::super::metrics::MetricsCollector,
    ) -> std::collections::HashMap<u32, u64> {
        metrics
            .generate_report()
            .consumer_delivery
            .into_iter()
            .map(|consumer| (consumer.ssrc, consumer.packets_by_second.iter().sum()))
            .collect()
    }

    async fn require_fresh_loopback_rtp(
        metrics: &super::super::metrics::MetricsCollector,
        tracks: &[LoopbackTrack],
    ) -> Result<()> {
        // Snapshot successful writer queues after renegotiation, not receiver
        // counts: packets still in flight from before the update cannot satisfy
        // this watermark. The fixture does not retransmit or reuse sequences.
        let watermarks: Vec<_> = tracks
            .iter()
            .map(|track| {
                (
                    track.ssrc,
                    track.queued.load(std::sync::atomic::Ordering::SeqCst),
                )
            })
            .collect();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut check = tokio::time::interval(Duration::from_millis(10));
            loop {
                check.tick().await;
                let after = loopback_packet_counts(metrics);
                if watermarks
                    .iter()
                    .all(|(ssrc, queued)| after.get(ssrc).copied().unwrap_or(0) >= queued + 5)
                {
                    return;
                }
            }
        })
        .await
        .context("fresh RTP did not arrive on every negotiated SSRC")
    }

    /// Two real loopback peers check the generator's receive delivery, not
    /// mediasoup interoperability, decoding quality, or server capacity.
    #[tokio::test]
    async fn receive_delivery_survives_incremental_consumer_renegotiation() {
        let metrics = Arc::new(super::super::metrics::MetricsCollector::new(
            "live-receiver".into(),
        ));
        metrics.begin_connection_attempt();
        let mut source: Option<Arc<dyn PeerConnection>> = None;
        let mut receiver: Option<WebRtcTransport> = None;
        let mut writer: Option<tokio::task::JoinHandle<Result<()>>> = None;

        // The sending fixture behaves as an ICE-lite/passive-DTLS endpoint,
        // allowing the real parameter-to-SDP receive path to be used unchanged.
        // Cleanup below runs after every Result error or elapsed deadline.
        let outcome =
            std::panic::AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(20), async {
                let mut engine = MediaEngine::default();
                for (kind, codec, payload_type) in [
                    (RtpCodecKind::Audio, audio_codec(), 111),
                    (RtpCodecKind::Video, video_codec(), 96),
                ] {
                    engine.register_codec(
                        RTCRtpCodecParameters {
                            rtp_codec: codec,
                            payload_type,
                        },
                        kind,
                    )?;
                }
                let mut settings =
                    rtc::peer_connection::configuration::setting_engine::SettingEngine::default();
                settings.set_lite(true);
                let (source_state_tx, source_state) =
                    tokio::sync::watch::channel(RTCPeerConnectionState::New);
                let (gathered_tx, mut gathered) = tokio::sync::watch::channel(false);
                let peer: Arc<dyn PeerConnection> = Arc::new(
                    PeerConnectionBuilder::new()
                        .with_media_engine(engine)
                        .with_setting_engine(settings)
                        .with_handler(Arc::new(LoopbackSenderEvents {
                            state: source_state_tx,
                            gathered: gathered_tx,
                        }))
                        .with_udp_addrs(vec!["127.0.0.1:0"])
                        .build()
                        .await?,
                );
                source = Some(peer.clone());
                let mut tracks = vec![
                    add_loopback_track(&peer, 0).await?,
                    add_loopback_track(&peer, 1).await?,
                ];
                peer.set_local_description(peer.create_offer(None).await?)
                    .await?;
                while !*gathered.borrow_and_update() {
                    gathered
                        .changed()
                        .await
                        .context("fixture gathering stopped")?;
                }
                let description = peer
                    .local_description()
                    .await
                    .context("fixture local SDP missing")?;
                let source_ice = loopback_ice_parameters(&description.sdp, true)?;
                let source_dtls = DtlsParameters {
                    role: DtlsRole::Server,
                    fingerprints: vec![extract_fingerprint_from_sdp(&description.sdp)?],
                };
                let stats = peer
                    .get_stats(std::time::Instant::now(), StatsSelector::None)
                    .await;
                let candidate_port = stats
                    .iter()
                    .find_map(|entry| {
                        if let RTCStatsReportEntry::LocalCandidate(candidate) = entry {
                            (candidate.address.as_deref() == Some("127.0.0.1"))
                                .then_some(candidate.port)
                        } else {
                            None
                        }
                    })
                    .context("fixture did not gather its loopback candidate")?;
                anyhow::ensure!(candidate_port > 0, "fixture candidate has no port");
                let mut source_candidate = candidate("127.0.0.1");
                source_candidate.port = candidate_port;

                let (recv, recv_dtls) = WebRtcTransport::new(
                    "live-receiver".into(),
                    "live-recv-transport".into(),
                    source_ice.clone(),
                    vec![source_candidate],
                    source_dtls.clone(),
                    false,
                    Some(metrics.clone()),
                )
                .await?;
                receiver = Some(recv);
                let recv = receiver.as_mut().unwrap();
                let recv_description = recv
                    .peer_connection
                    .local_description()
                    .await
                    .context("receiver local SDP missing")?;
                let recv_ice = loopback_ice_parameters(&recv_description.sdp, false)?;
                peer.set_remote_description(RTCSessionDescription::answer(generate_remote_sdp(
                    &recv_ice,
                    &recv_dtls,
                    true,
                    &[],
                )?)?)
                .await?;
                recv.set_remote_description(&source_ice, &source_dtls)
                    .await?;
                wait_for_connected(source_state.clone(), Duration::from_secs(5)).await?;
                wait_for_connected(recv.connection_state.clone(), Duration::from_secs(5)).await?;

                let (active_tx, active_rx) =
                    tokio::sync::watch::channel(Vec::<LoopbackTrack>::new());
                writer = Some(tokio::spawn(async move {
                    let mut pace = tokio::time::interval(Duration::from_millis(10));
                    pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut sequence = 0_u16;
                    loop {
                        pace.tick().await;
                        sequence = sequence.wrapping_add(1);
                        let active = active_rx.borrow().clone();
                        for track in active {
                            track
                                .track
                                .write_rtp(rtc::rtp::Packet {
                                    header: rtc::rtp::Header {
                                        version: 2,
                                        payload_type: if track.kind == MediaKind::Audio {
                                            111
                                        } else {
                                            96
                                        },
                                        sequence_number: sequence,
                                        timestamp: u32::from(sequence) * 480,
                                        ssrc: track.ssrc,
                                        ..Default::default()
                                    },
                                    payload: vec![0x10, 0x00, 0x01].into(),
                                })
                                .await?;
                            track
                                .queued
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }));

                for target in [2, 4, 6, 8] {
                    let previous = recv.consumers.len();
                    while tracks.len() < target {
                        tracks.push(add_loopback_track(&peer, tracks.len()).await?);
                    }
                    for track in &tracks[previous..] {
                        metrics.record_consumer(
                            &format!("consumer-{}", track.ssrc),
                            &format!("producer-{}", track.ssrc),
                            track.ssrc,
                        );
                        recv.add_consumer_info(
                            track.kind,
                            &RtpParameters {
                                encodings: vec![RtpEncodingParameters {
                                    ssrc: Some(track.ssrc),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        );
                    }
                    if target > 2 {
                        // Previously active tracks keep sending throughout both
                        // peers' incremental offer/answer updates.
                        peer.set_local_description(peer.create_offer(None).await?)
                            .await?;
                        peer.set_remote_description(RTCSessionDescription::answer(
                            generate_remote_sdp(&recv_ice, &recv_dtls, true, &recv.consumers)?,
                        )?)
                        .await?;
                    }
                    recv.renegotiate_consumers().await?;
                    active_tx.send_replace(tracks.clone());
                    require_fresh_loopback_rtp(&metrics, &tracks).await?;
                    anyhow::ensure!(
                        *source_state.borrow() == RTCPeerConnectionState::Connected
                            && *recv.connection_state.borrow() == RTCPeerConnectionState::Connected,
                        "renegotiation changed the connected transport state"
                    );
                }
                // Reapplying unchanged mappings must preserve existing track readers.
                recv.renegotiate_consumers().await?;
                require_fresh_loopback_rtp(&metrics, &tracks).await?;
                anyhow::ensure!(
                    metrics.generate_report().errors.is_empty(),
                    "receive path recorded an error"
                );
                Ok::<(), anyhow::Error>(())
            }))
            .catch_unwind()
            .await;

        let writer_result = if let Some(task) = writer {
            task.abort();
            Some(tokio::time::timeout(Duration::from_secs(2), task).await)
        } else {
            None
        };
        let receiver_closed = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(peer) = &receiver {
                peer.close().await?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let source_closed = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(peer) = &source {
                peer.close().await?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;

        receiver_closed
            .expect("receiver cleanup deadline")
            .expect("receiver cleanup failed");
        source_closed
            .expect("sender cleanup deadline")
            .expect("sender cleanup failed");
        if let Some(result) = writer_result {
            match result.expect("writer cleanup deadline") {
                Ok(result) => result.expect("RTP writer failed"),
                Err(error) => assert!(error.is_cancelled(), "RTP writer panicked: {error}"),
            }
        }
        let outcome = outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        outcome
            .expect("live renegotiation test deadline")
            .expect("live renegotiation failed");
    }

    #[test]
    fn diagnostics_sanitize_sdp_and_stats_using_allowlists() {
        let sdp = "v=0\r\na=ice-ufrag:secret-ufrag\r\na=ice-pwd:secret-password\r\na=fingerprint:sha-256 secret-fingerprint\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=recvonly\r\na=ssrc:123 cname:secret-cname\r\na=ssrc:123 msid:secret-msid\r\n\
            a=candidate:secret-candidate\r\nm=video 9 UDP/TLS/RTP/SAVPF 96 97\r\na=mid:1\r\na=sendonly\r\na=ssrc:456 cname:secret-cname\r\n";
        let media = serde_json::to_value(sanitize_sdp_media(sdp)).unwrap();
        assert_eq!(media[0]["mid"], 0);
        assert_eq!(media[0]["ssrcs"], serde_json::json!([123]));
        assert_eq!(media[1]["payloadTypes"], serde_json::json!([96, 97]));
        assert!(!media.to_string().contains("secret"));
        let stats = sanitize_rtc_stat(&serde_json::json!({
            "id": "transport", "type": "transport", "packetsReceived": 42,
            "dtlsState": "connected", "iceLocalUsernameFragment": "secret-ufrag",
            "password": "secret-password", "localCertificateId": "secret-fingerprint",
            "address": "secret-address", "futureCredentialField": "secret-future",
        }));
        assert_eq!(stats["packetsReceived"], 42);
        assert_eq!(stats["dtlsState"], "connected");
        assert_eq!(stats.as_object().unwrap().len(), 4);
        assert!(!stats.to_string().contains("secret"));
        let candidate = sanitize_rtc_stat(&serde_json::json!({
            "id": "candidate-1", "type": "local-candidate", "address": "127.0.0.1",
            "port": 45678, "protocol": "udp", "candidateType": "host",
            "usernameFragment": "secret-ufrag", "url": "secret-url",
        }));
        assert_eq!(candidate["port"], 45678);
        assert_eq!(candidate["isLoopback"], true);
        assert!(!candidate.to_string().contains("127.0.0.1"));
        assert!(!candidate.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn media_readiness_requires_connected_and_rejects_timeout_or_failure() {
        let (state, receiver) = tokio::sync::watch::channel(RTCPeerConnectionState::New);
        assert!(
            wait_for_connected(receiver.clone(), Duration::from_millis(5))
                .await
                .is_err()
        );
        let pending = tokio::spawn(wait_for_connected(receiver.clone(), Duration::from_secs(1)));
        state.send_replace(RTCPeerConnectionState::Connecting);
        tokio::task::yield_now().await;
        assert!(!pending.is_finished());
        state.send_replace(RTCPeerConnectionState::Connected);
        pending.await.unwrap().unwrap();
        for terminal in [
            RTCPeerConnectionState::Failed,
            RTCPeerConnectionState::Closed,
        ] {
            state.send_replace(terminal);
            assert!(
                wait_for_connected(receiver.clone(), Duration::from_secs(1))
                    .await
                    .is_err()
            );
        }
    }

    fn candidate(address: &str) -> IceCandidate {
        IceCandidate {
            foundation: "test".into(),
            priority: 2_130_706_431,
            address: address.into(),
            protocol: Protocol::Udp,
            port: 9,
            r#type: IceCandidateType::Host,
            tcp_type: None,
        }
    }

    fn parameters() -> (IceParameters, DtlsParameters) {
        (
            IceParameters {
                username_fragment: "testufrag".into(),
                password: "test-password-at-least-twenty-two-bytes".into(),
                ice_lite: Some(true),
            },
            DtlsParameters {
                role: DtlsRole::Server,
                fingerprints: vec![DtlsFingerprint::Sha256 { value: [0x42; 32] }],
            },
        )
    }

    #[test]
    fn local_candidate_addresses_preserve_loopback_and_ip_families() {
        assert_eq!(
            local_udp_addresses(&[candidate("127.0.0.1")]),
            vec!["127.0.0.1:0"]
        );
        assert_eq!(local_udp_addresses(&[candidate("::1")]), vec!["[::1]:0"]);
        assert_eq!(
            local_udp_addresses(&[candidate("127.0.0.1"), candidate("::1")]),
            vec!["127.0.0.1:0", "[::1]:0"]
        );
        assert_eq!(
            local_udp_addresses(&[candidate("192.0.2.1")]),
            vec!["0.0.0.0:0"]
        );
        assert_eq!(
            local_udp_addresses(&[candidate("2001:db8::1")]),
            vec!["0.0.0.0:0", "[::]:0"]
        );
    }

    #[tokio::test]
    async fn sender_offer_has_distinct_tracks_real_ssrcs_and_dtls_fingerprint() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (ice, dtls) = parameters();
            let (mut transport, local) = WebRtcTransport::new(
                "sender-test".into(),
                "send-transport".into(),
                ice.clone(),
                vec![candidate("127.0.0.1")],
                dtls.clone(),
                true,
                None,
            )
            .await
            .unwrap();
            assert!(matches!(
                local.fingerprints.as_slice(),
                [DtlsFingerprint::Sha256 { .. }]
            ));
            assert_eq!(local.role, DtlsRole::Client);
            let audio = transport.send_audio_track.as_ref().unwrap();
            let video = transport.send_video_track.as_ref().unwrap();
            assert_ne!(audio.track_id().await, video.track_id().await);
            assert_eq!(audio.stream_id().await, video.stream_id().await);
            let (audio_ssrc, video_ssrc) = transport.get_send_ssrcs().await.unwrap();
            assert_eq!(audio.ssrcs().await, vec![audio_ssrc]);
            assert_eq!(video.ssrcs().await, vec![video_ssrc]);
            let offer = transport
                .peer_connection
                .local_description()
                .await
                .unwrap()
                .sdp;
            for feedback in ["nack", "nack pli", "ccm fir"] {
                assert_eq!(
                    offer
                        .matches(&format!("a=rtcp-fb:96 {feedback}\r\n"))
                        .count(),
                    1
                );
            }
            transport.set_remote_description(&ice, &dtls).await.unwrap();
            let answer = transport
                .peer_connection
                .remote_description()
                .await
                .unwrap()
                .sdp;
            for feedback in ["nack", "nack pli", "ccm fir"] {
                assert!(answer.contains(&format!("a=rtcp-fb:96 {feedback}\r\n")));
            }
            assert!(!answer.contains("a=rtcp-fb:111"));
            transport.close().await.unwrap();
        })
        .await
        .expect("sender setup and cleanup must finish");
    }

    #[tokio::test]
    async fn receive_transport_accepts_batched_audio_video_consumers_and_renegotiation() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (ice, dtls) = parameters();
            let (mut transport, _) = WebRtcTransport::new(
                "receiver-test".into(),
                "receive-transport".into(),
                ice.clone(),
                vec![candidate("127.0.0.1")],
                dtls.clone(),
                false,
                None,
            )
            .await
            .unwrap();
            transport.set_remote_description(&ice, &dtls).await.unwrap();
            let rtp = |ssrc| RtpParameters {
                encodings: vec![RtpEncodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                }],
                ..Default::default()
            };
            transport.add_consumer_info(MediaKind::Audio, &rtp(1111));
            transport.add_consumer_info(MediaKind::Audio, &rtp(2222));
            transport.add_consumer_info(MediaKind::Video, &rtp(3333));
            transport.add_consumer_info(MediaKind::Audio, &rtp(1111));
            assert_eq!(
                transport.consumers.len(),
                3,
                "consumer replay must not allocate another transceiver"
            );
            assert_eq!(
                transport
                    .consumers
                    .iter()
                    .map(|consumer| consumer.mid)
                    .collect::<Vec<_>>(),
                vec![0, 2, 1]
            );
            transport.renegotiate_consumers().await.unwrap();
            transport.add_consumer_info(MediaKind::Video, &rtp(4444));
            transport.renegotiate_consumers().await.unwrap();
            assert_eq!(transport.recv_transceiver_count, 4);
            let snapshot = transport.diagnostic_snapshot().await.unwrap();
            assert_eq!(snapshot["transportId"], "receive-transport");
            assert_eq!(snapshot["consumerMappings"].as_array().unwrap().len(), 4);
            assert_eq!(snapshot["remoteMedia"].as_array().unwrap().len(), 4);
            assert!(
                snapshot["stats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["type"] == "transport")
            );
            assert!(!snapshot.to_string().contains("testufrag"));
            assert!(!snapshot.to_string().contains("test-password"));
            let answer = generate_remote_sdp(&ice, &dtls, false, &transport.consumers).unwrap();
            assert_eq!(answer.matches("m=audio ").count(), 2);
            assert_eq!(answer.matches("m=video ").count(), 2);
            assert!(answer.contains("a=group:BUNDLE 0 1 2 3\r\n"));
            for ssrc in [1111, 2222, 3333, 4444] {
                assert!(answer.contains(&format!("a=msid:mediasoup-{ssrc} consumer-{ssrc}\r\n")));
            }
            transport.close().await.unwrap();
        })
        .await
        .expect("receiver renegotiation and cleanup must finish");
    }

    #[tokio::test]
    async fn dropping_transport_cancels_pending_track_pollers_and_closes_peer() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (ice, dtls) = parameters();
            let (transport, _) = WebRtcTransport::new(
                "cancel-test".into(),
                "cancel-transport".into(),
                ice,
                vec![candidate("127.0.0.1")],
                dtls,
                false,
                None,
            )
            .await
            .unwrap();
            let peer = transport.peer_connection.clone();
            let mut cancellation = transport.cancellation.subscribe();
            let waiting = tokio::spawn(async move {
                next_track_event::<()>(&mut cancellation, std::future::pending()).await
            });
            tokio::task::yield_now().await;
            drop(transport);
            assert_eq!(
                waiting.await.unwrap(),
                None,
                "track polling must not depend on upstream OnEnded delivery"
            );
            loop {
                if peer.create_offer(None).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped peer must be closed and all pollers cancelled");
    }
}
