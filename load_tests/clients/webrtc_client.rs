// Real ICE/DTLS/RTP client using the webrtc-rs 0.20 async Sans-I/O driver.
// Mediasoup's parameter-based signaling is adapted to the peer's SDP API.

use anyhow::{Context, Result};
use mediasoup::prelude::*;
use mediasoup_types::data_structures::{DtlsFingerprint, DtlsRole, IceCandidateType};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RTCRtpHeaderExtensionCapability, RtpCodecKind,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use webrtc::media_stream::MediaStreamTrack;
use webrtc::media_stream::track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceConnectionState, RTCPeerConnectionState,
    RTCSessionDescription, Registry, register_default_interceptors,
};
use webrtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};

struct TransportEvents {
    client_id: String,
    transport_id: String,
    metrics: Option<Arc<super::metrics::MetricsCollector>>,
    cancellation: tokio::sync::watch::Receiver<bool>,
    connection_state: tokio::sync::watch::Sender<RTCPeerConnectionState>,
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
            state.changed().await.context("Media transport state channel closed")?;
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
        debug!(
            "{}: Transport {} ICE state: {:?}",
            self.client_id, self.transport_id, state
        );
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let client_id = self.client_id.clone();
        let metrics = self.metrics.clone();
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
                            }
                            metrics.record_packet_received(packet.payload.len());
                        }
                        if count % 500 == 0 {
                            debug!(
                                "{}: Received {} RTP packets (ssrc={})",
                                client_id, count, packet.header.ssrc
                            );
                        }
                    }
                    TrackRemoteEvent::OnEnded | TrackRemoteEvent::OnError => break,
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
        rtcp_feedback: vec![],
    }
}

async fn attach_local_track(
    peer: &Arc<dyn PeerConnection>,
    client_id: &str,
    kind: RtpCodecKind,
    codec: RTCRtpCodec,
    mut cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<Arc<TrackLocalStaticRTP>> {
    let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("stream-{client_id}"),
        format!("{kind}-{client_id}"),
        format!("{kind} load test"),
        kind,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(rand::random::<u32>()),
                ..Default::default()
            },
            codec,
            ..Default::default()
        }],
    )));
    peer.add_track(track.clone() as Arc<dyn TrackLocal>)
        .await
        .context("Failed to add local RTP track")?;
    // In 0.20 feedback is delivered on the local track, not on RtpSender.
    let feedback_track = track.clone();
    tokio::spawn(async move {
        while next_track_event(&mut cancellation, feedback_track.poll())
            .await
            .is_some()
        {}
    });
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
                ..Default::default()
            },
            RtpCodecKind::Audio,
        )?;
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: video_codec(),
                payload_type: 96,
                ..Default::default()
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
        let (connection_state_tx, connection_state) =
            tokio::sync::watch::channel(RTCPeerConnectionState::New);
        let handler = Arc::new(TransportEvents {
            client_id: client_id.clone(),
            transport_id: transport_id.clone(),
            metrics,
            cancellation: cancellation_rx,
            connection_state: connection_state_tx,
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
                            cancellation.subscribe(),
                        )
                        .await?,
                    ),
                    Some(
                        attach_local_track(
                            &peer_connection,
                            &client_id,
                            RtpCodecKind::Video,
                            video_codec(),
                            cancellation.subscribe(),
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

    /// Add an audio track for sending RTP.
    pub async fn add_audio_track(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        attach_local_track(
            &self.peer_connection,
            &self.client_id,
            RtpCodecKind::Audio,
            audio_codec(),
            self.cancellation.subscribe(),
        )
        .await
    }

    /// Add a video track for sending RTP.
    pub async fn add_video_track(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        attach_local_track(
            &self.peer_connection,
            &self.client_id,
            RtpCodecKind::Video,
            video_codec(),
            self.cancellation.subscribe(),
        )
        .await
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

    /// Add a consumer and immediately renegotiate (legacy single-consumer path)
    pub async fn add_consumer(
        &mut self,
        kind: MediaKind,
        consumer_rtp_parameters: &RtpParameters,
    ) -> Result<()> {
        self.add_consumer_info(kind, consumer_rtp_parameters);
        self.renegotiate_consumers().await
    }

    /// Get the peer connection for direct access
    pub fn peer_connection(&self) -> Arc<dyn PeerConnection> {
        Arc::clone(&self.peer_connection)
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
            } else if let Some(ssrc_str) = line.strip_prefix("a=ssrc:") {
                if let Some(ssrc_num_str) = ssrc_str.split_whitespace().next() {
                    if let Ok(ssrc) = ssrc_num_str.parse::<u32>() {
                        if in_audio && audio_ssrc.is_none() {
                            audio_ssrc = Some(ssrc);
                        } else if in_video && video_ssrc.is_none() {
                            video_ssrc = Some(ssrc);
                        }
                    }
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
            None, // send transport doesn't need metrics
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

    /// Get audio track (created during send transport setup)
    pub fn produce_audio(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        self.audio_track
            .clone()
            .context("No audio track (send transport not created?)")
    }

    /// Get video track (created during send transport setup)
    pub fn produce_video(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        self.video_track
            .clone()
            .context("No video track (send transport not created?)")
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
    use std::time::Duration;
    use webrtc::media_stream::Track;

    #[tokio::test]
    async fn media_readiness_requires_connected_and_rejects_timeout_or_failure() {
        let (state, receiver) = tokio::sync::watch::channel(RTCPeerConnectionState::New);
        assert!(wait_for_connected(receiver.clone(), Duration::from_millis(5)).await.is_err());
        let pending = tokio::spawn(wait_for_connected(receiver.clone(), Duration::from_secs(1)));
        state.send_replace(RTCPeerConnectionState::Connecting);
        tokio::task::yield_now().await;
        assert!(!pending.is_finished());
        state.send_replace(RTCPeerConnectionState::Connected);
        pending.await.unwrap().unwrap();
        for terminal in [RTCPeerConnectionState::Failed, RTCPeerConnectionState::Closed] {
            state.send_replace(terminal);
            assert!(wait_for_connected(receiver.clone(), Duration::from_secs(1)).await.is_err());
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
            transport.set_remote_description(&ice, &dtls).await.unwrap();
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
