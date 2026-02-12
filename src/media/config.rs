#![forbid(unsafe_code)]

// Configuration for mediasoup workers, routers, and transports

use mediasoup::prelude::*;
use mediasoup::worker::{WorkerLogLevel, WorkerLogTag, WorkerDtlsFiles};
use std::num::NonZeroU8;

/// Main media server configuration
#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub worker_config: WorkerConfig,
    pub router_config: RouterConfig,
    pub webrtc_transport_config: WebRtcTransportConfig,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            worker_config: WorkerConfig::default(),
            router_config: RouterConfig::default(),
            webrtc_transport_config: WebRtcTransportConfig::default(),
        }
    }
}

/// Worker configuration
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub num_workers: usize,
    pub log_level: WorkerLogLevel,
    pub log_tags: Vec<WorkerLogTag>,
    pub rtc_min_port: u16,
    pub rtc_max_port: u16,
    pub dtls_certificate_file: Option<String>,
    pub dtls_private_key_file: Option<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            num_workers: num_cpus::get(),
            log_level: WorkerLogLevel::Warn,
            log_tags: vec![
                WorkerLogTag::Info,
                WorkerLogTag::Ice,
                WorkerLogTag::Dtls,
                WorkerLogTag::Rtp,
                WorkerLogTag::Rtcp,
            ],
            rtc_min_port: 10000,
            rtc_max_port: 59999,
            dtls_certificate_file: None,
            dtls_private_key_file: None,
        }
    }
}

impl WorkerConfig {
    /// Converts to mediasoup WorkerSettings
    pub fn to_worker_settings(&self) -> WorkerSettings {
        let mut settings = WorkerSettings::default();

        settings.log_level = self.log_level;
        settings.log_tags = self.log_tags.clone();
        settings.rtc_port_range = self.rtc_min_port..=self.rtc_max_port;

        if let (Some(cert), Some(key)) = (&self.dtls_certificate_file, &self.dtls_private_key_file) {
            settings.dtls_files = Some(WorkerDtlsFiles {
                certificate: cert.clone().into(),
                private_key: key.clone().into(),
            });
        }

        settings
    }
}

/// Router configuration with codec capabilities
#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub media_codecs: Vec<RtpCodecCapability>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            media_codecs: Self::default_codecs(),
        }
    }
}

impl RouterConfig {
    /// Returns default codec capabilities for audio and video
    pub fn default_codecs() -> Vec<RtpCodecCapability> {
        vec![
            // Audio codecs
            RtpCodecCapability::Audio {
                mime_type: MimeTypeAudio::Opus,
                preferred_payload_type: Some(111),
                clock_rate: NonZeroU32::new(48000).unwrap(),
                channels: NonZeroU8::new(2).unwrap(),
                parameters: RtpCodecParametersParameters::from([
                    ("minptime", 10_u32.into()),
                    ("useinbandfec", 1_u32.into()),
                ]),
                rtcp_feedback: vec![
                    RtcpFeedback::TransportCc,
                ],
            },
            // Video codecs - VP8
            RtpCodecCapability::Video {
                mime_type: MimeTypeVideo::Vp8,
                preferred_payload_type: Some(96),
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![
                    RtcpFeedback::Nack,
                    RtcpFeedback::NackPli,
                    RtcpFeedback::CcmFir,
                    RtcpFeedback::GoogRemb,
                    RtcpFeedback::TransportCc,
                ],
            },
            // Video codecs - VP9
            RtpCodecCapability::Video {
                mime_type: MimeTypeVideo::Vp9,
                preferred_payload_type: Some(98),
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![
                    RtcpFeedback::Nack,
                    RtcpFeedback::NackPli,
                    RtcpFeedback::CcmFir,
                    RtcpFeedback::GoogRemb,
                    RtcpFeedback::TransportCc,
                ],
            },
            // Video codecs - H264
            RtpCodecCapability::Video {
                mime_type: MimeTypeVideo::H264,
                preferred_payload_type: Some(102),
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::from([
                    ("level-asymmetry-allowed", 1_u32.into()),
                    ("packetization-mode", 1_u32.into()),
                    ("profile-level-id", "42e01f".into()),
                ]),
                rtcp_feedback: vec![
                    RtcpFeedback::Nack,
                    RtcpFeedback::NackPli,
                    RtcpFeedback::CcmFir,
                    RtcpFeedback::GoogRemb,
                    RtcpFeedback::TransportCc,
                ],
            },
        ]
    }
    
    /// Converts to RouterOptions for mediasoup
    pub fn to_router_options(&self) -> RouterOptions {
        RouterOptions::new(self.media_codecs.clone())
    }
}

/// WebRTC transport configuration
#[derive(Debug, Clone)]
pub struct WebRtcTransportConfig {
    pub listen_ips: Vec<ListenInfo>,
    pub initial_available_outgoing_bitrate: u32,
    pub min_outgoing_bitrate: u32,
    pub max_outgoing_bitrate: u32,
    pub max_incoming_bitrate: Option<u32>,
    pub enable_udp: bool,
    pub enable_tcp: bool,
    pub prefer_udp: bool,
    pub prefer_tcp: bool,
}

impl Default for WebRtcTransportConfig {
    fn default() -> Self {
        Self {
            listen_ips: vec![
                ListenInfo {
                    protocol: Protocol::Udp,
                    ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                    announced_address: None,
                    port: None,
                    port_range: None,
                    flags: None,
                    send_buffer_size: None,
                    recv_buffer_size: None,
                    expose_internal_ip: false,
                },
            ],
            initial_available_outgoing_bitrate: 600_000,
            min_outgoing_bitrate: 100_000,
            max_outgoing_bitrate: 3_000_000,
            max_incoming_bitrate: Some(1_500_000),
            enable_udp: true,
            enable_tcp: true,
            prefer_udp: true,
            prefer_tcp: false,
        }
    }
}

impl WebRtcTransportConfig {
    /// Sets the public IP address for the transport
    pub fn with_public_ip(mut self, public_ip: IpAddr) -> Self {
        if let Some(listen_ip) = self.listen_ips.first_mut() {
            listen_ip.announced_address = Some(public_ip.to_string());
        }
        self
    }

    /// Converts to WebRtcTransportOptions
    pub fn to_transport_options(&self) -> WebRtcTransportOptions {
        // Use the first listen IP, or create a default one
        let listen_info = self.listen_ips.first().cloned().unwrap_or_else(|| ListenInfo {
            protocol: Protocol::Udp,
            ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            announced_address: None,
            port: None,
            port_range: None,
            flags: None,
            send_buffer_size: None,
            recv_buffer_size: None,
            expose_internal_ip: false,
        });
        WebRtcTransportOptions::new(WebRtcTransportListenInfos::new(listen_info))
    }
}

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
