#![forbid(unsafe_code)]

// Configuration for mediasoup workers, routers, and transports

use mediasoup::prelude::*;
use mediasoup::worker::{WorkerDtlsFiles, WorkerLogLevel, WorkerLogTag};
use std::num::NonZeroU8;

pub const DEFAULT_WEBRTC_SERVER_PORT_BASE: u16 = 40_000;
pub const MAX_MEDIA_WORKERS: usize = 64;
/// Socket buffer each media worker asks the kernel for on its WebRTC
/// listener, in bytes; 0 keeps the kernel default (208 KiB on Linux). That
/// default holds about 14 ms of the inbound media of 200 publishers, and at
/// it the kernel sheds millisecond bursts throughout a loaded room (1–4 % of
/// the primary worker's inbound datagrams on the production shape), which is
/// what cost handshakes their final flight; 1 MiB (about 70 ms) absorbs
/// those bursts (no drop in three of four runs). It does not rescue a worker
/// that falls behind for seconds: that regime drops a quarter of the inbound
/// at any buffer size, and a much deeper buffer only adds queueing delay
/// (`docs/performance-results.md`). The kernel clamps the request to
/// `net.core.rmem_max`/`wmem_max`, so a host must raise those for the size
/// to apply (`docs/deployment.md`).
pub const DEFAULT_WEBRTC_SOCKET_BUFFER_BYTES: u32 = 1024 * 1024;
const MIN_WEBRTC_SOCKET_BUFFER_BYTES: u32 = 64 * 1024;
/// One keyframe request per second per video stream, in milliseconds.
const DEFAULT_KEYFRAME_REQUEST_DELAY_MS: u32 = 1_000;
const MAX_WEBRTC_SOCKET_BUFFER_BYTES: u32 = 64 * 1024 * 1024;

/// Main media server configuration
#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub worker_config: WorkerConfig,
    pub router_config: RouterConfig,
    pub webrtc_transport_config: WebRtcTransportConfig,
    /// Base UDP port for WebRtcServer per worker. Worker i listens on base + i.
    pub webrtc_server_port_base: u16,
    /// Also listen for ICE-TCP on each worker's port. Off by default: the
    /// deployment must publish the TCP range and open it at the firewall,
    /// otherwise clients would be offered candidates that cannot connect.
    pub webrtc_server_tcp: bool,
    /// Receive buffer requested for each worker's listeners, in bytes; 0 keeps
    /// the kernel default. `WEBRTC_RECV_BUFFER_BYTES`.
    pub webrtc_recv_buffer_bytes: u32,
    /// Send buffer requested for each worker's listeners, in bytes; 0 keeps
    /// the kernel default. `WEBRTC_SEND_BUFFER_BYTES`.
    pub webrtc_send_buffer_bytes: u32,
    /// Video producers pass keyframe requests to their sender at most once per
    /// this many milliseconds; 0 forwards every one. Each viewer that starts
    /// consuming asks for a keyframe, and the keyframe goes to every viewer of
    /// the stream: forwarding all of them collapsed viewers' bandwidth
    /// estimates in a 545-viewer webinar, where 1 s kept them while joiners
    /// waited under a second for video. `MEDIA_KEYFRAME_REQUEST_DELAY_MS`.
    pub key_frame_request_delay_ms: u32,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            worker_config: WorkerConfig::default(),
            router_config: RouterConfig::default(),
            webrtc_transport_config: WebRtcTransportConfig::default(),
            webrtc_server_port_base: DEFAULT_WEBRTC_SERVER_PORT_BASE,
            webrtc_server_tcp: false,
            webrtc_recv_buffer_bytes: DEFAULT_WEBRTC_SOCKET_BUFFER_BYTES,
            webrtc_send_buffer_bytes: DEFAULT_WEBRTC_SOCKET_BUFFER_BYTES,
            key_frame_request_delay_ms: DEFAULT_KEYFRAME_REQUEST_DELAY_MS,
        }
    }
}

impl MediaConfig {
    /// Builds the media configuration from process settings.
    ///
    /// `MEDIA_WORKERS` is optional. When present it must be an integer from 1
    /// through 64. When absent, the worker count defaults to the detected CPU
    /// count, capped to the same range.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::default();

        match std::env::var("MEDIA_WORKERS") {
            Ok(value) => {
                config.worker_config.num_workers = parse_media_workers(&value)?;
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("MEDIA_WORKERS must be valid UTF-8");
            }
        }
        if let Ok(value) = std::env::var("MEDIA_WORKER_LOG_LEVEL") {
            config.worker_config.log_level = parse_worker_log_level(&value)?;
        }
        if let Ok(value) = std::env::var("MEDIA_WORKER_LOG_TAGS") {
            config.worker_config.log_tags = parse_worker_log_tags(&value)?;
        }
        if let Ok(value) = std::env::var("LIBWEBRTC_FIELD_TRIALS") {
            config.worker_config.libwebrtc_field_trials = Some(parse_field_trials(&value)?);
        }

        match std::env::var("WEBRTC_SERVER_PORT_BASE") {
            Ok(value) => config.webrtc_server_port_base = parse_webrtc_port_base(&value)?,
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("WEBRTC_SERVER_PORT_BASE must be valid UTF-8")
            }
        }
        if let Ok(value) = std::env::var("WEBRTC_SERVER_TCP") {
            config.set_tcp(parse_switch("WEBRTC_SERVER_TCP", &value)?);
        }
        if let Ok(value) = std::env::var("WEBRTC_RECV_BUFFER_BYTES") {
            config.webrtc_recv_buffer_bytes =
                parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", &value)?;
        }
        if let Ok(value) = std::env::var("WEBRTC_SEND_BUFFER_BYTES") {
            config.webrtc_send_buffer_bytes =
                parse_socket_buffer("WEBRTC_SEND_BUFFER_BYTES", &value)?;
        }
        if let Ok(value) = std::env::var("MEDIA_KEYFRAME_REQUEST_DELAY_MS") {
            config.key_frame_request_delay_ms = parse_keyframe_request_delay(&value)?;
        }
        if let Ok(value) = std::env::var("WEBRTC_MIN_OUTGOING_BITRATE") {
            config.webrtc_transport_config.min_outgoing_bitrate =
                parse_bitrate("WEBRTC_MIN_OUTGOING_BITRATE", &value)?;
        }
        if let Ok(value) = std::env::var("WEBRTC_MAX_INCOMING_BITRATE") {
            config.webrtc_transport_config.max_incoming_bitrate =
                Some(parse_bitrate("WEBRTC_MAX_INCOMING_BITRATE", &value)?).filter(|v| *v > 0);
        }

        config.validate()?;
        Ok(config)
    }

    /// Enables or disables ICE-TCP on every worker's port. The transport flag
    /// follows the listener: mediasoup builds a transport's candidates from
    /// the server's listeners, so the flag alone never produces one.
    pub fn set_tcp(&mut self, tcp: bool) {
        self.webrtc_server_tcp = tcp;
        self.webrtc_transport_config.enable_tcp = tcp;
    }

    /// Validates worker capacity and transport settings before opening listeners.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_worker_count(self.worker_config.num_workers)?;
        self.worker_port(self.worker_config.num_workers - 1)?;
        self.webrtc_transport_config.validate()?;
        anyhow::ensure!(
            !self.webrtc_transport_config.enable_tcp || self.webrtc_server_tcp,
            "ICE-TCP requires the shared WebRTC server's TCP listener"
        );
        Ok(())
    }

    /// Returns the dedicated WebRTC UDP port for a worker without allowing
    /// integer truncation or wraparound.
    pub fn worker_port(&self, worker_index: usize) -> anyhow::Result<u16> {
        checked_worker_port(
            self.webrtc_server_port_base,
            self.worker_config.num_workers,
            worker_index,
        )
    }
}

/// A libwebrtc field-trial string: `Name/Value/` pairs, letters, digits and
/// a few punctuation characters, so a typo cannot smuggle anything else into
/// the worker's configuration.
/// `MEDIA_WORKER_LOG_LEVEL`: what the native worker logs (debug, warn, error or none).
fn parse_worker_log_level(value: &str) -> anyhow::Result<WorkerLogLevel> {
    match value.trim() {
        "debug" => Ok(WorkerLogLevel::Debug),
        "warn" => Ok(WorkerLogLevel::Warn),
        "error" => Ok(WorkerLogLevel::Error),
        "none" => Ok(WorkerLogLevel::None),
        other => {
            anyhow::bail!("MEDIA_WORKER_LOG_LEVEL must be debug, warn, error or none, not {other}")
        }
    }
}

/// `MEDIA_WORKER_LOG_TAGS`: the native worker's log tags, comma separated.
fn parse_worker_log_tags(value: &str) -> anyhow::Result<Vec<WorkerLogTag>> {
    let mut tags = Vec::new();
    for name in value.split(',') {
        let tag = match name.trim() {
            "info" => WorkerLogTag::Info,
            "ice" => WorkerLogTag::Ice,
            "dtls" => WorkerLogTag::Dtls,
            "rtp" => WorkerLogTag::Rtp,
            "srtp" => WorkerLogTag::Srtp,
            "rtcp" => WorkerLogTag::Rtcp,
            "rtx" => WorkerLogTag::Rtx,
            "bwe" => WorkerLogTag::Bwe,
            "score" => WorkerLogTag::Score,
            "simulcast" => WorkerLogTag::Simulcast,
            "svc" => WorkerLogTag::Svc,
            "sctp" => WorkerLogTag::Sctp,
            "message" => WorkerLogTag::Message,
            other => anyhow::bail!("Unknown MEDIA_WORKER_LOG_TAGS entry: {other:?}"),
        };
        tags.push(tag);
    }
    Ok(tags)
}

fn parse_field_trials(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 2048 {
        anyhow::bail!("LIBWEBRTC_FIELD_TRIALS must be 1-2048 characters");
    }
    if !value.ends_with('/') || value.split('/').filter(|part| !part.is_empty()).count() % 2 != 0 {
        anyhow::bail!("LIBWEBRTC_FIELD_TRIALS must be Name/Value/ pairs ending in a slash");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | ':' | ',' | '.'))
    {
        anyhow::bail!("LIBWEBRTC_FIELD_TRIALS may contain only letters, digits and / - _ : , .");
    }
    Ok(value.to_string())
}

fn parse_switch(name: &str, value: &str) -> anyhow::Result<bool> {
    match value.trim() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => anyhow::bail!("{name} must be true or false, not {other}"),
    }
}

/// A bitrate in bits per second from 0 (mediasoup's own default) to 50 Mbit/s.
fn parse_bitrate(name: &str, value: &str) -> anyhow::Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|bitrate| *bitrate <= 50_000_000)
        .ok_or_else(|| anyhow::anyhow!("{name} must be an integer from 0 through 50000000 bit/s"))
}

/// The keyframe request coalescing window: 0 through 10 s.
fn parse_keyframe_request_delay(value: &str) -> anyhow::Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|ms| *ms <= 10_000)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "MEDIA_KEYFRAME_REQUEST_DELAY_MS must be an integer from 0 through 10000 ms"
            )
        })
}

/// A socket buffer size in bytes: 0 for the kernel default, otherwise 64 KiB
/// through 64 MiB.
fn parse_socket_buffer(name: &str, value: &str) -> anyhow::Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|bytes| {
            *bytes == 0
                || (MIN_WEBRTC_SOCKET_BUFFER_BYTES..=MAX_WEBRTC_SOCKET_BUFFER_BYTES).contains(bytes)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{name} must be 0 or an integer from {MIN_WEBRTC_SOCKET_BUFFER_BYTES} through {MAX_WEBRTC_SOCKET_BUFFER_BYTES} bytes"
            )
        })
}

/// The kernel silently clamps a socket buffer request to `net.core.rmem_max`
/// or `wmem_max`, so a worker that asks for the configured size gets the
/// host's ceiling instead. Returns that ceiling when it is below the request.
pub fn clamped_socket_buffer(requested: u32, kernel_max: Option<u64>) -> Option<u64> {
    kernel_max.filter(|max| u64::from(requested) > *max)
}

/// `net.core.rmem_max` or `wmem_max` as the kernel reports it; `None` where
/// `/proc/sys` is absent (not Linux) or unreadable.
pub fn kernel_socket_buffer_max(sysctl: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/sys/net/core/{sysctl}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn parse_webrtc_port_base(value: &str) -> anyhow::Result<u16> {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| {
            anyhow::anyhow!("WEBRTC_SERVER_PORT_BASE must be an integer from 1 through 65535")
        })
}

fn parse_media_workers(value: &str) -> anyhow::Result<usize> {
    let value = value.trim();
    let count = value.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("MEDIA_WORKERS must be an integer from 1 through {MAX_MEDIA_WORKERS}")
    })?;
    validate_worker_count(count)?;
    Ok(count)
}

fn validate_worker_count(count: usize) -> anyhow::Result<()> {
    if !(1..=MAX_MEDIA_WORKERS).contains(&count) {
        anyhow::bail!("media worker count must be from 1 through {MAX_MEDIA_WORKERS}, got {count}");
    }
    Ok(())
}

pub(crate) fn checked_worker_port(
    port_base: u16,
    worker_count: usize,
    worker_index: usize,
) -> anyhow::Result<u16> {
    validate_worker_count(worker_count)?;
    if worker_index >= worker_count {
        anyhow::bail!(
            "media worker index {worker_index} is outside configured worker count {worker_count}"
        );
    }

    let offset = u16::try_from(worker_index)
        .map_err(|_| anyhow::anyhow!("media worker index {worker_index} exceeds u16"))?;
    port_base.checked_add(offset).ok_or_else(|| {
        anyhow::anyhow!(
            "WebRTC worker port range overflows u16: base {port_base}, worker index {worker_index}"
        )
    })
}

/// Worker configuration
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub num_workers: usize,
    pub log_level: WorkerLogLevel,
    pub log_tags: Vec<WorkerLogTag>,
    /// libwebrtc field trials for the worker's congestion controller; `None`
    /// keeps mediasoup's default (`WebRTC-Bwe-AlrLimitedBackoff/Enabled/`).
    pub libwebrtc_field_trials: Option<String>,
    pub rtc_min_port: u16,
    pub rtc_max_port: u16,
    pub dtls_certificate_file: Option<String>,
    pub dtls_private_key_file: Option<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            num_workers: num_cpus::get().clamp(1, MAX_MEDIA_WORKERS),
            log_level: WorkerLogLevel::Warn,
            libwebrtc_field_trials: None,
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
        if let Some(trials) = &self.libwebrtc_field_trials {
            settings.libwebrtc_field_trials = Some(trials.clone());
        }
        settings.rtc_port_range = self.rtc_min_port..=self.rtc_max_port;

        if let (Some(cert), Some(key)) = (&self.dtls_certificate_file, &self.dtls_private_key_file)
        {
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
                rtcp_feedback: vec![RtcpFeedback::Nack, RtcpFeedback::TransportCc],
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
    /// Application-imposed outgoing floor in bits per second. Zero leaves the
    /// native congestion controller's minimum (30 kbit/s) unchanged; a
    /// positive override keeps sending above a weak link's estimate, which
    /// measurably keeps the lowest layer flowing at a 150 kbit/s cap.
    /// `WEBRTC_MIN_OUTGOING_BITRATE`.
    pub min_outgoing_bitrate: u32,
    pub max_outgoing_bitrate: u32,
    /// REMB ceiling sent to each publisher. It must cover the largest
    /// simulcast ladder the client publishes (1080p: about 2.9 Mbit/s), or
    /// the browser starves the top layer. `WEBRTC_MAX_INCOMING_BITRATE`.
    pub max_incoming_bitrate: Option<u32>,
    pub enable_udp: bool,
    /// Offer ICE-TCP candidates. Set through `MediaConfig::set_tcp` so it
    /// never claims a listener the WebRtcServer does not have.
    pub enable_tcp: bool,
    pub prefer_udp: bool,
    pub prefer_tcp: bool,
}

impl Default for WebRtcTransportConfig {
    fn default() -> Self {
        Self {
            listen_ips: vec![ListenInfo {
                protocol: Protocol::Udp,
                ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                announced_address: None,
                port: None,
                port_range: None,
                flags: None,
                send_buffer_size: None,
                recv_buffer_size: None,
                expose_internal_ip: false,
            }],
            initial_available_outgoing_bitrate: 600_000,
            min_outgoing_bitrate: 100_000,
            max_outgoing_bitrate: 3_000_000,
            max_incoming_bitrate: Some(3_000_000),
            enable_udp: true,
            enable_tcp: false,
            prefer_udp: true,
            prefer_tcp: false,
        }
    }
}

impl WebRtcTransportConfig {
    /// Reject policies the native worker cannot apply. Zero preserves the native
    /// default minimum or disables the maximum, as in the mediasoup API.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, bitrate) in [
            ("minimum outgoing bitrate", self.min_outgoing_bitrate),
            ("maximum outgoing bitrate", self.max_outgoing_bitrate),
        ] {
            anyhow::ensure!(
                bitrate == 0 || bitrate >= 30_000,
                "{name} must be zero or at least 30000 bit/s"
            );
        }
        anyhow::ensure!(
            self.max_outgoing_bitrate == 0
                || self.min_outgoing_bitrate <= self.max_outgoing_bitrate,
            "minimum outgoing bitrate must not exceed maximum outgoing bitrate"
        );
        anyhow::ensure!(
            self.enable_udp || self.enable_tcp,
            "at least one WebRTC transport protocol must be enabled"
        );
        Ok(())
    }

    /// Sets the public IP address for the transport
    pub fn with_public_ip(mut self, public_ip: IpAddr) -> Self {
        if let Some(listen_ip) = self.listen_ips.first_mut() {
            listen_ip.announced_address = Some(public_ip.to_string());
        }
        self
    }
}

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_log_level_and_tags_parse_the_documented_names_only() {
        assert!(matches!(
            parse_worker_log_level(" debug ").unwrap(),
            WorkerLogLevel::Debug
        ));
        assert!(matches!(
            parse_worker_log_level("warn").unwrap(),
            WorkerLogLevel::Warn
        ));
        assert!(matches!(
            parse_worker_log_level("error").unwrap(),
            WorkerLogLevel::Error
        ));
        assert!(matches!(
            parse_worker_log_level("none").unwrap(),
            WorkerLogLevel::None
        ));
        assert!(parse_worker_log_level("verbose").is_err());
        assert_eq!(
            parse_worker_log_tags("ice, dtls").unwrap(),
            vec![WorkerLogTag::Ice, WorkerLogTag::Dtls]
        );
        assert_eq!(
            parse_worker_log_tags("info,rtp,rtcp,srtp,rtx,bwe,score,simulcast,svc,sctp,message")
                .unwrap()
                .len(),
            11
        );
        assert!(parse_worker_log_tags("").is_err());
        assert!(parse_worker_log_tags("ice,packets").is_err());
    }

    #[test]
    fn default_bitrate_policy_preserves_existing_limits() {
        let config = WebRtcTransportConfig::default();
        assert_eq!(config.min_outgoing_bitrate, 100_000);
        assert_eq!(config.initial_available_outgoing_bitrate, 600_000);
        assert_eq!(config.max_outgoing_bitrate, 3_000_000);
        // 100k + 300k + 2.5M for the 1080p ladder, plus audio, must fit under
        // the publisher's REMB ceiling or the browser starves the top layer.
        assert_eq!(config.max_incoming_bitrate, Some(3_000_000));
        assert!(config.max_incoming_bitrate.unwrap() >= 100_000 + 300_000 + 2_500_000 + 64_000);
    }

    #[test]
    fn tcp_switch_parses_and_moves_both_listener_and_transport_flag() {
        assert!(parse_switch("WEBRTC_SERVER_TCP", " true ").unwrap());
        assert!(!parse_switch("WEBRTC_SERVER_TCP", "0").unwrap());
        assert!(parse_switch("WEBRTC_SERVER_TCP", "maybe").is_err());
        let mut config = MediaConfig::default();
        assert!(!config.webrtc_server_tcp);
        assert!(!config.webrtc_transport_config.enable_tcp);
        config.set_tcp(true);
        assert!(config.webrtc_server_tcp && config.webrtc_transport_config.enable_tcp);
        config.set_tcp(false);
        assert!(!config.webrtc_server_tcp && !config.webrtc_transport_config.enable_tcp);
    }

    #[test]
    fn bitrate_overrides_are_bounded_and_zero_means_mediasoup_default() {
        assert_eq!(parse_bitrate("X", " 0 ").unwrap(), 0);
        assert_eq!(parse_bitrate("X", "30000").unwrap(), 30_000);
        assert!(parse_bitrate("X", "50000001").is_err());
        assert!(parse_bitrate("X", "-1").is_err());
        assert!(parse_bitrate("X", "fast").is_err());
    }

    #[test]
    fn transport_policies_fail_validation_before_native_creation() {
        let mut config = MediaConfig::default();
        for minimum in [1, 29_999, 3_000_001, 50_000_000] {
            config.webrtc_transport_config.min_outgoing_bitrate = minimum;
            assert!(config.validate().is_err(), "invalid floor {minimum}");
        }
        for minimum in [0, 30_000, 100_000, 3_000_000] {
            config.webrtc_transport_config.min_outgoing_bitrate = minimum;
            assert!(config.validate().is_ok(), "valid floor {minimum}");
        }
        config.webrtc_transport_config.max_outgoing_bitrate = 0;
        assert!(config.validate().is_ok(), "zero removes the outgoing cap");
        config.webrtc_transport_config.max_outgoing_bitrate = 29_999;
        assert!(config.validate().is_err());
        config.webrtc_transport_config = WebRtcTransportConfig::default();
        config.webrtc_transport_config.enable_udp = false;
        assert!(config.validate().is_err(), "a transport needs a protocol");
        config.webrtc_transport_config.enable_tcp = true;
        assert!(config.validate().is_err(), "TCP needs a listener");
        config.set_tcp(true);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn opus_negotiates_nack_alongside_transport_cc() {
        let codecs = RouterConfig::default_codecs();
        let opus_feedback = codecs
            .iter()
            .find_map(|codec| match codec {
                RtpCodecCapability::Audio {
                    mime_type: MimeTypeAudio::Opus,
                    rtcp_feedback,
                    ..
                } => Some(rtcp_feedback),
                _ => None,
            })
            .expect("Opus is advertised");
        assert!(opus_feedback.contains(&RtcpFeedback::Nack));
        assert!(opus_feedback.contains(&RtcpFeedback::TransportCc));
    }

    #[test]
    fn media_worker_count_is_bounded() {
        assert_eq!(parse_media_workers("1").unwrap(), 1);
        assert_eq!(parse_media_workers(" 64 ").unwrap(), 64);
        assert!(parse_media_workers("0").is_err());
        assert!(parse_media_workers("65").is_err());
        assert!(parse_media_workers("many").is_err());
        assert!((1..=MAX_MEDIA_WORKERS).contains(&WorkerConfig::default().num_workers));
    }

    #[test]
    fn media_port_base_is_explicit_and_bounded() {
        assert_eq!(
            parse_field_trials(
                " WebRTC-Bwe-AlrLimitedBackoff/Enabled/WebRTC-BweBackOffFactor/Enabled-0.92/ "
            )
            .unwrap(),
            "WebRTC-Bwe-AlrLimitedBackoff/Enabled/WebRTC-BweBackOffFactor/Enabled-0.92/"
        );
        assert!(parse_field_trials("WebRTC-Bwe-AlrLimitedBackoff/Enabled").is_err());
        assert!(parse_field_trials("WebRTC-Bwe-AlrLimitedBackoff/").is_err());
        assert!(parse_field_trials("WebRTC-X/Enabled; rm -rf/").is_err());
        assert!(parse_field_trials("").is_err());
        assert_eq!(parse_webrtc_port_base("41000").unwrap(), 41000);
        assert!(parse_webrtc_port_base("0").is_err());
        assert!(parse_webrtc_port_base("65536").is_err());
        assert!(parse_webrtc_port_base("ports").is_err());
    }

    #[test]
    fn socket_buffers_default_to_one_mebibyte_and_parse_within_bounds() {
        let config = MediaConfig::default();
        assert_eq!(config.webrtc_recv_buffer_bytes, 1024 * 1024);
        assert_eq!(config.webrtc_send_buffer_bytes, 1024 * 1024);
        assert_eq!(
            parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", " 8388608 ").unwrap(),
            8_388_608
        );
        assert_eq!(
            parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", "0").unwrap(),
            0
        );
        assert!(parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", "1024").is_err());
        assert!(parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", "67108865").is_err());
        assert!(parse_socket_buffer("WEBRTC_RECV_BUFFER_BYTES", "4M").is_err());
    }

    #[test]
    fn keyframe_request_delay_parses_within_bounds() {
        assert_eq!(MediaConfig::default().key_frame_request_delay_ms, 1_000);
        assert_eq!(parse_keyframe_request_delay(" 1000 ").unwrap(), 1000);
        assert_eq!(parse_keyframe_request_delay("0").unwrap(), 0);
        assert_eq!(parse_keyframe_request_delay("10000").unwrap(), 10_000);
        assert!(parse_keyframe_request_delay("10001").is_err());
        assert!(parse_keyframe_request_delay("-1").is_err());
        assert!(parse_keyframe_request_delay("1s").is_err());
    }

    #[test]
    fn a_kernel_ceiling_below_the_request_is_reported_and_zero_never_is() {
        assert_eq!(
            clamped_socket_buffer(1024 * 1024, Some(212_992)),
            Some(212_992)
        );
        assert_eq!(clamped_socket_buffer(1024 * 1024, Some(2_097_152)), None);
        assert_eq!(clamped_socket_buffer(1024 * 1024, None), None);
        assert_eq!(clamped_socket_buffer(0, Some(1)), None);
        assert_eq!(kernel_socket_buffer_max("no_such_sysctl"), None);
    }

    #[test]
    fn worker_ports_are_checked_and_match_published_range() {
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = MAX_MEDIA_WORKERS;

        assert_eq!(config.worker_port(0).unwrap(), 40_000);
        assert_eq!(config.worker_port(63).unwrap(), 40_063);
        assert!(config.worker_port(64).is_err());
        assert!(config.validate().is_ok());

        config.webrtc_server_port_base = u16::MAX - 1;
        assert!(config.validate().is_err());
    }
}
