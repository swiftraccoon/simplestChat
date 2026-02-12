use std::time::Duration;
use std::num::{NonZeroU32, NonZeroU8};
use mediasoup::prelude::*;

/// Configuration for synthetic media generation
#[derive(Debug, Clone)]
pub struct MediaConfig {
    pub audio_enabled: bool,
    pub video_enabled: bool,
    pub audio_codec: String,      // "opus"
    pub video_codec: String,      // "VP8" or "H264"
    pub audio_sample_rate: u32,   // 48000 Hz
    pub audio_channels: u8,       // 1 or 2
    pub video_width: u32,         // 640, 1280, etc.
    pub video_height: u32,        // 480, 720, etc.
    pub video_fps: u8,            // 30
    pub video_bitrate_kbps: u32,  // 500, 1000, etc.
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            audio_enabled: true,
            video_enabled: true,
            audio_codec: "opus".to_string(),
            video_codec: "VP8".to_string(),
            audio_sample_rate: 48000,
            audio_channels: 2,
            video_width: 640,
            video_height: 480,
            video_fps: 30,
            video_bitrate_kbps: 500,
        }
    }
}

impl MediaConfig {
    pub fn audio_only() -> Self {
        Self {
            audio_enabled: true,
            video_enabled: false,
            ..Default::default()
        }
    }

    pub fn video_only() -> Self {
        Self {
            audio_enabled: false,
            video_enabled: true,
            ..Default::default()
        }
    }
}

/// Generates synthetic media packets
pub struct MediaGenerator {
    config: MediaConfig,
    audio_sequence: u16,
    video_sequence: u16,
    audio_timestamp: u32,
    video_timestamp: u32,
    audio_ssrc: u32,
    video_ssrc: u32,
    frame_count: u64,
}

impl MediaGenerator {
    pub fn new(config: MediaConfig) -> Self {
        // Generate random SSRCs (in real implementation, these would be negotiated)
        let audio_ssrc = rand::random::<u32>();
        let video_ssrc = rand::random::<u32>();

        Self {
            config,
            audio_sequence: 0,
            video_sequence: 0,
            audio_timestamp: 0,
            video_timestamp: 0,
            audio_ssrc,
            video_ssrc,
            frame_count: 0,
        }
    }

    /// Generate RTP parameters for audio producer
    pub fn generate_audio_rtp_parameters(&self, _router_caps: &RtpCapabilitiesFinalized) -> RtpParameters {
        // Use standard Opus parameters
        RtpParameters {
            mid: None,
            codecs: vec![RtpCodecParameters::Audio {
                mime_type: MimeTypeAudio::Opus,
                payload_type: 111, // Standard Opus payload type
                clock_rate: NonZeroU32::new(self.config.audio_sample_rate).unwrap(),
                channels: NonZeroU8::new(self.config.audio_channels).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            header_extensions: vec![],
            encodings: vec![RtpEncodingParameters {
                ssrc: Some(self.audio_ssrc),
                ..Default::default()
            }],
            rtcp: RtcpParameters::default(),
        }
    }

    /// Generate RTP parameters for video producer
    pub fn generate_video_rtp_parameters(&self, _router_caps: &RtpCapabilitiesFinalized) -> RtpParameters {
        // Use standard VP8/H264 parameters
        let codec_params = if self.config.video_codec == "VP8" {
            RtpCodecParameters::Video {
                mime_type: MimeTypeVideo::Vp8,
                payload_type: 96, // Standard VP8 payload type
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }
        } else {
            RtpCodecParameters::Video {
                mime_type: MimeTypeVideo::H264,
                payload_type: 102, // Standard H264 payload type
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }
        };

        RtpParameters {
            mid: None,
            codecs: vec![codec_params],
            header_extensions: vec![],
            encodings: vec![RtpEncodingParameters {
                ssrc: Some(self.video_ssrc),
                max_bitrate: Some(self.config.video_bitrate_kbps * 1000),
                ..Default::default()
            }],
            rtcp: RtcpParameters::default(),
        }
    }

    /// Generate a synthetic audio RTP packet (Opus, 20ms packet)
    ///
    /// Includes one-byte RTP header extension (RFC 5285) with MID extension so
    /// mediasoup can remap it for consumers. Without MID in forwarded packets,
    /// webrtc-rs on the recv side cannot route video to the correct transceiver.
    pub fn generate_audio_packet(&mut self) -> Vec<u8> {
        // Opus typically uses 20ms packets at 48kHz = 960 samples
        // Payload size varies (compressed), typically 20-100 bytes for speech
        let payload_size = 60; // Simulated Opus frame size

        let mut packet = Vec::with_capacity(12 + 8 + payload_size);

        // RTP Header (12 bytes)
        packet.push(0x90); // V=2, P=0, X=1, CC=0 (X=1 for header extension)
        packet.push(0x6F); // M=0, PT=111 (typical Opus payload type)

        // Sequence number (2 bytes, big endian)
        packet.extend_from_slice(&self.audio_sequence.to_be_bytes());
        self.audio_sequence = self.audio_sequence.wrapping_add(1);

        // Timestamp (4 bytes, big endian) - increments by 960 for 20ms @ 48kHz
        packet.extend_from_slice(&self.audio_timestamp.to_be_bytes());
        self.audio_timestamp = self.audio_timestamp.wrapping_add(960);

        // SSRC (4 bytes, big endian)
        packet.extend_from_slice(&self.audio_ssrc.to_be_bytes());

        // One-byte RTP header extension (RFC 5285)
        // Profile: 0xBEDE, Length: 1 word (4 bytes of extension data)
        packet.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01]);
        // MID extension: ID=1, L=0 (1 byte value), value='0' (audio mid)
        packet.push(0x10); // (ID=1 << 4) | (length-1=0)
        packet.push(b'0');  // mid value "0" for audio m-section
        packet.push(0x00); // padding
        packet.push(0x00); // padding

        // Synthetic Opus payload (simulated compressed audio)
        packet.extend(std::iter::repeat(0xAA).take(payload_size));

        packet
    }

    /// Generate a synthetic video RTP packet with valid VP8 payload descriptor
    ///
    /// VP8 RTP payload format (RFC 7741):
    ///   Byte 0: [X|R|N|S|R|PID] - payload descriptor
    ///   Byte 1+: VP8 frame data (P bit in LSB: 0=keyframe, 1=interframe)
    ///
    /// mediasoup's C++ worker parses the VP8 payload descriptor and will crash
    /// on malformed payloads (free(): invalid pointer). We must produce valid
    /// descriptor bytes and a correct keyframe signature.
    pub fn generate_video_packet(&mut self) -> Vec<u8> {
        let is_keyframe = self.frame_count % 150 == 0; // Keyframe every 5 seconds @ 30fps
        let payload_size = if is_keyframe { 2000 } else { 500 };

        let mut packet = Vec::with_capacity(12 + 8 + payload_size);

        // RTP Header (12 bytes)
        let marker_bit = if is_keyframe { 0x80 } else { 0x00 };
        packet.push(0x90); // V=2, P=0, X=1, CC=0 (X=1 for header extension)
        packet.push(0x60 | marker_bit); // M bit on keyframes, PT=96

        // Sequence number
        packet.extend_from_slice(&self.video_sequence.to_be_bytes());
        self.video_sequence = self.video_sequence.wrapping_add(1);

        // Timestamp (4 bytes) - increments by 3000 for 30fps @ 90kHz clock
        packet.extend_from_slice(&self.video_timestamp.to_be_bytes());
        let timestamp_increment = 90000 / self.config.video_fps as u32;
        self.video_timestamp = self.video_timestamp.wrapping_add(timestamp_increment);

        // SSRC
        packet.extend_from_slice(&self.video_ssrc.to_be_bytes());

        // One-byte RTP header extension (RFC 5285)
        // Profile: 0xBEDE, Length: 1 word (4 bytes of extension data)
        packet.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01]);
        // MID extension: ID=1, L=0 (1 byte value), value='1' (video mid)
        packet.push(0x10); // (ID=1 << 4) | (length-1=0)
        packet.push(b'1');  // mid value "1" for video m-section
        packet.push(0x00); // padding
        packet.push(0x00); // padding

        // VP8 RTP payload descriptor (RFC 7741) with Picture ID.
        // mediasoup's C++ PayloadDescriptorHandler requires a picture ID to
        // properly rewrite descriptors during forwarding. Without it, the
        // handler can segfault when accessing NULL picture ID state.
        //
        // Byte 0: X=1, S=1, PID=0 → 0x90
        // Byte 1: I=1, L=0, T=0, K=0 → 0x80
        // Byte 2: M=0, PictureID (7-bit) → 0x00-0x7F
        let pic_id = (self.frame_count & 0x7F) as u8;
        packet.push(0x90); // X=1, S=1, PID=0
        packet.push(0x80); // I=1 (picture ID present)
        packet.push(pic_id); // 7-bit picture ID

        // VP8 frame data
        if is_keyframe {
            packet.push(0x10); // frame_type=0 (key), version=0, show_frame=1
            packet.push(0x00); // first_part_size continuation
            packet.push(0x00); // first_part_size continuation
            packet.push(0x9D); // VP8 keyframe start code
            packet.push(0x01);
            packet.push(0x2A);
            packet.extend_from_slice(&[0x80, 0x02]); // width=640
            packet.extend_from_slice(&[0xE0, 0x01]); // height=480
            packet.extend(std::iter::repeat(0x00).take(payload_size - 12));
        } else {
            packet.push(0x11); // frame_type=1 (inter), version=0, show_frame=1
            packet.push(0x00); // first_part_size continuation
            packet.push(0x00); // first_part_size continuation
            packet.extend(std::iter::repeat(0x00).take(payload_size - 6));
        }

        self.frame_count += 1;

        packet
    }

    /// Get the audio SSRC used in generated packets
    pub fn audio_ssrc(&self) -> u32 {
        self.audio_ssrc
    }

    /// Get the video SSRC used in generated packets
    pub fn video_ssrc(&self) -> u32 {
        self.video_ssrc
    }

    /// Get the interval between audio packets (20ms for Opus)
    pub fn audio_packet_interval(&self) -> Duration {
        Duration::from_millis(20)
    }

    /// Get the interval between video frames
    pub fn video_packet_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.config.video_fps as f64)
    }
}

// Simple random generator for SSRCs
mod rand {
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEED: AtomicU32 = AtomicU32::new(0x12345678);

    pub fn random<T: From<u32>>() -> T {
        let mut x = SEED.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        SEED.store(x, Ordering::Relaxed);
        T::from(x)
    }
}
