// Production-grade WebRTC client implementation using webrtc-rs 0.17
//
// This implementation establishes real ICE/DTLS/RTP connections with mediasoup.
// It converts mediasoup's parameter-based signaling to webrtc-rs's SDP-based API.

use anyhow::{Context, Result};
use mediasoup::prelude::*;
use mediasoup_types::data_structures::{DtlsFingerprint, DtlsRole, IceCandidateType};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability, RTPCodecType,
};
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocal;

/// WebRTC transport for mediasoup integration
///
/// This handles a single mediasoup transport (send or recv) using webrtc-rs.
/// The key challenge is that webrtc-rs uses SDP while mediasoup uses parameter-based signaling.
pub struct WebRtcTransport {
    peer_connection: Arc<RTCPeerConnection>,
    transport_id: String,
    client_id: String,
    is_send: bool,
    /// ICE candidates from mediasoup (kept for renegotiation)
    ice_candidate_inits: Vec<RTCIceCandidateInit>,
    /// Stored for recv transport SDP renegotiation
    ice_parameters: IceParameters,
    dtls_parameters: DtlsParameters,
    /// Consumer SSRCs tracked for SDP renegotiation
    consumers: Vec<ConsumerInfo>,
    /// Send transport tracks (created before SDP negotiation so they get bound)
    send_audio_track: Option<Arc<TrackLocalStaticRTP>>,
    send_video_track: Option<Arc<TrackLocalStaticRTP>>,
}

#[derive(Clone)]
struct ConsumerInfo {
    kind: MediaKind,
    ssrc: u32,
    mid_ext_id: Option<u16>,
}

impl WebRtcTransport {
    /// Creates a new WebRTC transport for mediasoup
    ///
    /// For recv transports, pass `Some(metrics)` to directly increment packet
    /// counters from the on_track handler (no intermediate channel).
    pub async fn new(
        client_id: String,
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
        is_send: bool,
        metrics: Option<Arc<super::metrics::MetricsCollector>>,
    ) -> Result<(Self, DtlsParameters)> {
        debug!(
            "{}: Creating WebRTC transport {} (send={})",
            client_id, transport_id, is_send
        );

        // Create MediaEngine with codecs matching mediasoup
        let mut media_engine = MediaEngine::default();

        // Register Opus codec for audio (standard mediasoup config)
        media_engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: "audio/opus".to_string(),
                        clock_rate: 48000,
                        channels: 2,
                        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                        rtcp_feedback: vec![],
                    },
                    payload_type: 111,
                    ..Default::default()
                },
                RTPCodecType::Audio,
            )
            .context("Failed to register Opus codec")?;

        // Register VP8 codec for video (standard mediasoup config)
        media_engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: "video/VP8".to_string(),
                        clock_rate: 90000,
                        channels: 0,
                        sdp_fmtp_line: String::new(),
                        rtcp_feedback: vec![],
                    },
                    payload_type: 96,
                    ..Default::default()
                },
                RTPCodecType::Video,
            )
            .context("Failed to register VP8 codec")?;

        // Register mid header extension so webrtc-rs can route incoming RTP
        // packets to the correct transceiver. Without this, on_track never fires.
        media_engine
            .register_header_extension(
                RTCRtpHeaderExtensionCapability {
                    uri: "urn:ietf:params:rtp-hdrext:sdes:mid".to_owned(),
                },
                RTPCodecType::Audio,
                None,
            )
            .context("Failed to register mid extension for audio")?;

        media_engine
            .register_header_extension(
                RTCRtpHeaderExtensionCapability {
                    uri: "urn:ietf:params:rtp-hdrext:sdes:mid".to_owned(),
                },
                RTPCodecType::Video,
                None,
            )
            .context("Failed to register mid extension for video")?;

        // Create interceptor registry for RTCP handling using default
        let registry = register_default_interceptors(
            Default::default(),
            &mut media_engine,
        )?;

        // Build WebRTC API
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // Create peer connection (no STUN needed for direct LAN)
        let config = RTCConfiguration {
            ice_servers: vec![],
            ..Default::default()
        };

        let peer_connection = Arc::new(
            api.new_peer_connection(config)
                .await
                .context("Failed to create peer connection")?,
        );

        // Set up connection state monitoring
        let client_id_clone = client_id.clone();
        let transport_id_clone = transport_id.clone();
        peer_connection.on_peer_connection_state_change(Box::new(
            move |state: RTCPeerConnectionState| {
                let cid = client_id_clone.clone();
                let tid = transport_id_clone.clone();
                Box::pin(async move {
                    match state {
                        RTCPeerConnectionState::Connected => {
                            info!("{}: Transport {} connected", cid, tid);
                        }
                        RTCPeerConnectionState::Disconnected => {
                            warn!("{}: Transport {} disconnected", cid, tid);
                        }
                        RTCPeerConnectionState::Failed => {
                            error!("{}: Transport {} failed", cid, tid);
                        }
                        RTCPeerConnectionState::Closed => {
                            debug!("{}: Transport {} closed", cid, tid);
                        }
                        _ => {}
                    }
                })
            },
        ));

        // Set up ICE connection state monitoring
        let client_id_clone = client_id.clone();
        let transport_id_clone = transport_id.clone();
        peer_connection.on_ice_connection_state_change(Box::new(
            move |state: RTCIceConnectionState| {
                let cid = client_id_clone.clone();
                let tid = transport_id_clone.clone();
                Box::pin(async move {
                    debug!("{}: Transport {} ICE state: {:?}", cid, tid, state);
                })
            },
        ));

        // Build ICE candidate inits to add later (after remote description is set)
        let mut pending_ice_candidates = Vec::new();
        for candidate in ice_candidates {
            let protocol_str = match candidate.protocol {
                Protocol::Udp => "udp",
                Protocol::Tcp => "tcp",
            };
            let type_str = match candidate.r#type {
                IceCandidateType::Host => "host",
                IceCandidateType::Srflx => "srflx",
                IceCandidateType::Prflx => "prflx",
                IceCandidateType::Relay => "relay",
            };
            let tcp_type_str = candidate
                .tcp_type
                .as_ref()
                .map(|_| " tcptype passive".to_string())
                .unwrap_or_default();

            let candidate_string = format!(
                "candidate:{} 1 {} {} {} {} typ {}{}",
                candidate.foundation,
                protocol_str,
                candidate.priority,
                candidate.address,
                candidate.port,
                type_str,
                tcp_type_str,
            );

            pending_ice_candidates.push(RTCIceCandidateInit {
                candidate: candidate_string,
                ..Default::default()
            });
        }

        // For recv transport: add recvonly transceivers and set up on_track handler
        // BEFORE creating the initial offer, so the offer has m-lines for audio + video.
        if !is_send {
            let init = webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            };
            peer_connection
                .add_transceiver_from_kind(RTPCodecType::Audio, Some(init))
                .await
                .context("Failed to add audio recv transceiver")?;

            let init = webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            };
            peer_connection
                .add_transceiver_from_kind(RTPCodecType::Video, Some(init))
                .await
                .context("Failed to add video recv transceiver")?;

            // Set up a single on_track handler for ALL incoming tracks.
            // This fires when webrtc-rs detects an incoming SSRC that matches the remote SDP.
            //
            // CRITICAL: webrtc-rs's do_track() holds a Mutex on the handler while awaiting
            // the returned future. If this future blocks (e.g. infinite read loop), the mutex
            // is held forever and subsequent tracks can never fire on_track. We MUST spawn a
            // separate task for the read loop and return immediately to release the mutex.
            let client_id_clone = client_id.clone();
            let metrics_clone = metrics.clone();
            peer_connection.on_track(Box::new(
                move |track, _receiver, _transceiver| {
                    let cid = client_id_clone.clone();
                    let m = metrics_clone.clone();
                    Box::pin(async move {
                        let ssrc = track.ssrc();
                        info!(
                            "{}: on_track fired: kind={}, codec={}, ssrc={}",
                            cid,
                            track.kind(),
                            track.codec().capability.mime_type,
                            ssrc
                        );
                        // Spawn the read loop in a separate task so this future returns
                        // immediately, releasing the on_track_handler mutex for the next track.
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 1500];
                            let mut count = 0u64;
                            loop {
                                match track.read(&mut buf).await {
                                    Ok((packet, _attrs)) => {
                                        count += 1;
                                        // Directly increment metrics atomics — no channel overhead
                                        if let Some(ref metrics) = m {
                                            if count == 1 {
                                                metrics.mark_first_media_received();
                                            }
                                            metrics.record_packet_received(packet.payload.len());
                                        }
                                        if count % 500 == 0 {
                                            debug!(
                                                "{}: Received {} RTP packets (ssrc={})",
                                                cid, count, ssrc
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        debug!("{}: Track read error: {}", cid, e);
                                        break;
                                    }
                                }
                            }
                            info!(
                                "{}: Track stopped (ssrc={}), received {} packets total",
                                cid, ssrc, count
                            );
                        });
                    })
                },
            ));
        }

        // For send transport: add tracks BEFORE SDP negotiation so they appear in the
        // initial offer and get properly bound. Without this, TrackLocalStaticRTP::write()
        // silently drops all packets because there are no bindings.
        let mut send_audio_track = None;
        let mut send_video_track = None;
        if is_send {
            let audio_track = Arc::new(TrackLocalStaticRTP::new(
                RTCRtpCodecCapability {
                    mime_type: "audio/opus".to_string(),
                    clock_rate: 48000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                    rtcp_feedback: vec![],
                },
                format!("audio-{}", client_id),
                format!("stream-{}", client_id),
            ));

            let rtp_sender = peer_connection
                .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>)
                .await
                .context("Failed to add audio track to send transport")?;

            let client_id_clone = client_id.clone();
            tokio::spawn(async move {
                let mut rtcp_buf = vec![0u8; 1500];
                while let Ok((_, _)) = rtp_sender.read(&mut rtcp_buf).await {}
                debug!("{}: Audio RTCP handler stopped", client_id_clone);
            });

            let video_track = Arc::new(TrackLocalStaticRTP::new(
                RTCRtpCodecCapability {
                    mime_type: "video/VP8".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: String::new(),
                    rtcp_feedback: vec![],
                },
                format!("video-{}", client_id),
                format!("stream-{}", client_id),
            ));

            let rtp_sender = peer_connection
                .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
                .await
                .context("Failed to add video track to send transport")?;

            let client_id_clone = client_id.clone();
            tokio::spawn(async move {
                let mut rtcp_buf = vec![0u8; 1500];
                while let Ok((_, _)) = rtp_sender.read(&mut rtcp_buf).await {}
                debug!("{}: Video RTCP handler stopped", client_id_clone);
            });

            send_audio_track = Some(audio_track);
            send_video_track = Some(video_track);
            info!("{}: Audio and video tracks added to send transport before SDP", client_id);
        }

        let transport = Self {
            peer_connection,
            transport_id,
            client_id,
            is_send,
            ice_candidate_inits: pending_ice_candidates,
            ice_parameters,
            dtls_parameters,
            consumers: Vec::new(),
            send_audio_track,
            send_video_track,
        };

        // Generate local DTLS parameters by creating an offer/answer
        let local_dtls = transport.generate_dtls_parameters().await?;

        Ok((transport, local_dtls))
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

    /// Add an audio track for sending RTP
    pub async fn add_audio_track(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        let track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: "audio/opus".to_string(),
                clock_rate: 48000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                rtcp_feedback: vec![],
            },
            format!("audio-{}", self.client_id),
            format!("stream-{}", self.client_id),
        ));

        let rtp_sender = self
            .peer_connection
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .context("Failed to add audio track")?;

        // Spawn RTCP handler
        let client_id = self.client_id.clone();
        tokio::spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            while let Ok((_, _)) = rtp_sender.read(&mut rtcp_buf).await {
                // Handle RTCP feedback
            }
            debug!("{}: Audio RTCP handler stopped", client_id);
        });

        info!("{}: Audio track added", self.client_id);
        Ok(track)
    }

    /// Add a video track for sending RTP
    pub async fn add_video_track(&self) -> Result<Arc<TrackLocalStaticRTP>> {
        let track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: "video/VP8".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            format!("video-{}", self.client_id),
            format!("stream-{}", self.client_id),
        ));

        let rtp_sender = self
            .peer_connection
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .context("Failed to add video track")?;

        // Spawn RTCP handler
        let client_id = self.client_id.clone();
        tokio::spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            while let Ok((_, _)) = rtp_sender.read(&mut rtcp_buf).await {
                // Handle RTCP feedback
            }
            debug!("{}: Video RTCP handler stopped", client_id);
        });

        info!("{}: Video track added", self.client_id);
        Ok(track)
    }

    /// Record a consumer for later SDP renegotiation (does NOT renegotiate yet).
    ///
    /// Call `renegotiate_consumers()` after all consumers are recorded to do a
    /// single SDP renegotiation that registers ALL SSRCs at once. This avoids
    /// a webrtc-rs bug where the second `set_remote_description` during rapid
    /// renegotiation doesn't properly register new SSRCs.
    pub fn add_consumer_info(
        &mut self,
        kind: MediaKind,
        consumer_rtp_parameters: &RtpParameters,
    ) {
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
            self.client_id, kind, ssrc, mid_ext_id,
            consumer_rtp_parameters.mid,
        );

        self.consumers.push(ConsumerInfo {
            kind,
            ssrc,
            mid_ext_id,
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
    pub fn peer_connection(&self) -> Arc<RTCPeerConnection> {
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

        transport
            .set_remote_description(&ice_parameters, &dtls_parameters)
            .await?;

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

        transport
            .set_remote_description(&ice_parameters, &dtls_parameters)
            .await?;

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
        let transport = self
            .send_transport
            .as_ref()
            .context("No send transport")?;
        transport.get_send_ssrcs().await
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
        if let Some(t) = &self.send_transport {
            t.close().await?;
        }
        if let Some(t) = &self.recv_transport {
            t.close().await?;
        }
        Ok(())
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
                    _ => Err(anyhow::anyhow!("Unsupported fingerprint algorithm or length")),
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

    // Media direction from the remote (answerer/mediasoup) perspective
    let direction = if is_send { "recvonly" } else { "sendonly" };

    let mut sdp = String::new();

    // Session-level attributes
    sdp.push_str(&format!(
        "v=0\r\n\
         o=- 0 0 IN IP4 0.0.0.0\r\n\
         s=-\r\n\
         t=0 0\r\n\
         a=group:BUNDLE 0 1\r\n\
         a=ice-ufrag:{}\r\n\
         a=ice-pwd:{}\r\n",
        ice_parameters.username_fragment,
        ice_parameters.password,
    ));

    // Audio m-section
    sdp.push_str(&format!(
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
         c=IN IP4 0.0.0.0\r\n\
         a=rtcp:9 IN IP4 0.0.0.0\r\n\
         a=rtcp-mux\r\n\
         a=mid:0\r\n\
         a={direction}\r\n\
         a=rtpmap:111 opus/48000/2\r\n\
         a=fmtp:111 minptime=10;useinbandfec=1\r\n\
         a=extmap:1 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
         a=fingerprint:{fp_algorithm} {fp_value}\r\n\
         a=setup:{setup}\r\n",
    ));

    // Add ALL audio consumer SSRCs
    for consumer in consumers.iter().filter(|c| c.kind == MediaKind::Audio) {
        sdp.push_str(&format!("a=ssrc:{} cname:mediasoup\r\n", consumer.ssrc));
    }

    // Video m-section
    sdp.push_str(&format!(
        "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
         c=IN IP4 0.0.0.0\r\n\
         a=rtcp:9 IN IP4 0.0.0.0\r\n\
         a=rtcp-mux\r\n\
         a=mid:1\r\n\
         a={direction}\r\n\
         a=rtpmap:96 VP8/90000\r\n\
         a=extmap:1 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
         a=fingerprint:{fp_algorithm} {fp_value}\r\n\
         a=setup:{setup}\r\n",
    ));

    // Add ALL video consumer SSRCs
    for consumer in consumers.iter().filter(|c| c.kind == MediaKind::Video) {
        sdp.push_str(&format!("a=ssrc:{} cname:mediasoup\r\n", consumer.ssrc));
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
