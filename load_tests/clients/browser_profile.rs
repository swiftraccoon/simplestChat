// Browser-faithful synthetic participants.
//
// A capacity figure is only as good as its workload. The web client publishes
// three simulcast layers, consumes every remote producer up to the server's
// per-participant cap, asks for the layer its tile can show and sends almost
// nothing while its microphone hears silence (Opus DTX). The synthetic default
// profile does none of this, so it measures a different, much cheaper room.
// Every constant below names the web client code it mirrors: change both
// together, and re-run the calibration when either moves.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

/// Camera capture the web client requests (`web/src/media.ts`,
/// `captureConstraints`). 720p is its default; 360p is not modelled because
/// browsers send fewer simulcast layers at that size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Capture {
    #[serde(rename = "720p")]
    Hd,
    #[serde(rename = "1080p")]
    FullHd,
}

impl Capture {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "720p" => Ok(Self::Hd),
            "1080p" => Ok(Self::FullHd),
            other => anyhow::bail!("--capture must be 720p or 1080p, not {other}"),
        }
    }
}

/// One simulcast encoding as `ensureVideoProducer` publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulcastLayer {
    pub rid: &'static str,
    pub width: u32,
    pub height: u32,
    pub max_bitrate_bps: u32,
}

/// r0 at a quarter of the capture size and 100 kbit/s, r1 at half size and
/// 300 kbit/s, r2 at full size and 900 kbit/s (2.5 Mbit/s for a 1080p
/// capture), lowest first because mediasoup numbers spatial layers by
/// encoding order.
pub fn camera_layers(capture: Capture) -> [SimulcastLayer; 3] {
    let (width, height, top_bitrate) = match capture {
        Capture::Hd => (1280, 720, 900_000),
        Capture::FullHd => (1920, 1080, 2_500_000),
    };
    [
        SimulcastLayer {
            rid: "r0",
            width: width / 4,
            height: height / 4,
            max_bitrate_bps: 100_000,
        },
        SimulcastLayer {
            rid: "r1",
            width: width / 2,
            height: height / 2,
            max_bitrate_bps: 300_000,
        },
        SimulcastLayer {
            rid: "r2",
            width,
            height,
            max_bitrate_bps: top_bitrate,
        },
    ]
}

/// Room screen layout (`web/src/style.css`): classic, the default, keeps a
/// 220 px roster and a 320 px chat beside the video grid; modern keeps only
/// the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Classic,
    Modern,
}

impl Layout {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "classic" => Ok(Self::Classic),
            "modern" => Ok(Self::Modern),
            other => anyhow::bail!("--layout must be classic or modern, not {other}"),
        }
    }

    fn side_panels(self) -> f64 {
        match self {
            Self::Classic => 220.0 + 320.0,
            Self::Modern => 320.0,
        }
    }
}

/// Device-pixel widths the three layers deliver (`web/src/layer-cap.ts`).
const LAYER_WIDTHS: [f64; 3] = [320.0, 640.0, 1280.0];
/// A tile steps up only when it clearly outgrows its layer and down only well
/// below the next one (`UP_MARGIN` and `DOWN_MARGIN` in `web/src/layer-cap.ts`).
const UP_MARGIN: f64 = 1.15;
const DOWN_MARGIN: f64 = 0.85;

/// `spatialLayerForRenderedWidth`: the layer a tile `rendered_width` CSS pixels
/// wide needs at `pixel_ratio`, given the layer it has. The shared cases in
/// `web/tests/layer-cap-cases.json` hold both implementations to one answer.
pub fn layer_for_width(rendered_width: f64, pixel_ratio: f64, current: Option<u8>) -> u8 {
    let pixel_ratio = if pixel_ratio > 0.0 {
        pixel_ratio.max(1.0)
    } else {
        1.0
    };
    let needed = rendered_width.max(0.0) * pixel_ratio;
    let top = LAYER_WIDTHS.len() - 1;
    let target = LAYER_WIDTHS
        .iter()
        .position(|width| needed <= *width)
        .unwrap_or(top);
    let Some(current) = current.map(usize::from).filter(|current| *current <= top) else {
        return target as u8;
    };
    let layer = if target > current {
        // Move up only when the tile exceeds the current layer's width by the margin.
        if needed > LAYER_WIDTHS[current] * UP_MARGIN {
            target
        } else {
            current
        }
    } else if target < current {
        // Move down only when the tile fits the lower layer with room to spare.
        if needed < LAYER_WIDTHS[target] * DOWN_MARGIN {
            target
        } else {
            current
        }
    } else {
        current
    };
    layer as u8
}

/// A browser's video grid as its consumers arrive. The web client draws a tile
/// per remote participant whose media it consumes, and its own while it
/// publishes; every layout re-evaluates each video tile's layer with the
/// hysteresis above (`observeTileSize` in `web/src/main.ts`), so tiles that
/// appeared while the grid was small keep a higher layer after it fills.
/// Departures are not modelled: calibration rooms only fill.
#[derive(Debug)]
pub struct BrowserGrid {
    viewer: Viewer,
    own_tile: bool,
    owners: HashMap<String, String>,
    participants: HashSet<String>,
    layers: BTreeMap<String, u8>,
}

impl BrowserGrid {
    pub fn new(viewer: Viewer, own_tile: bool) -> Self {
        Self {
            viewer,
            own_tile,
            owners: HashMap::new(),
            participants: HashSet::new(),
            layers: BTreeMap::new(),
        }
    }

    /// Remember whose producer this is, as `NewProducer` announces it.
    pub fn announce(&mut self, producer_id: &str, participant_id: &str) {
        self.owners
            .insert(producer_id.to_string(), participant_id.to_string());
    }

    /// Lay out one batch of resumed consumers, `(consumer, producer, is_video)`,
    /// and return the layer requests the web client would send: every new
    /// video tile's, and each existing one whose layer the new size changes.
    pub fn consume(&mut self, batch: &[(String, String, bool)]) -> Vec<(String, u8)> {
        for (_, producer, _) in batch {
            let owner = self
                .owners
                .get(producer)
                .cloned()
                .unwrap_or_else(|| producer.clone());
            self.participants.insert(owner);
        }
        let tiles = usize::from(self.own_tile) + self.participants.len();
        let width = self.viewer.tile_width(tiles);
        let mut requests = Vec::new();
        for (consumer, current) in &mut self.layers {
            let layer = layer_for_width(width, self.viewer.pixel_ratio, Some(*current));
            if layer != *current {
                *current = layer;
                requests.push((consumer.clone(), layer));
            }
        }
        for (consumer, _, video) in batch {
            if *video && !self.layers.contains_key(consumer) {
                let layer = layer_for_width(width, self.viewer.pixel_ratio, None);
                self.layers.insert(consumer.clone(), layer);
                requests.push((consumer.clone(), layer));
            }
        }
        requests
    }
}
/// `#video-grid` padding and gap, and the `minmax(280px, 1fr)` tile floor
/// of its `data-count='many'` rule.
const GRID_PADDING: f64 = 8.0;
const GRID_GAP: f64 = 8.0;
const MIN_TILE_WIDTH: f64 = 280.0;
/// At 768 CSS pixels and below the room screen stacks into one column, which
/// this model does not describe.
pub const MIN_DESKTOP_VIEWPORT: u32 = 769;

/// The screen each synthetic viewer pretends to have: it decides which
/// simulcast layer the web client's tile-size cap requests.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewer {
    pub viewport_width: u32,
    pub pixel_ratio: f64,
    pub layout: Layout,
}

impl Viewer {
    /// Rendered width in CSS pixels of each tile when the grid holds `tiles`
    /// tiles (the local tile included, as `updateVideoGridCount` counts them).
    pub fn tile_width(&self, tiles: usize) -> f64 {
        let grid =
            (f64::from(self.viewport_width) - self.layout.side_panels() - 2.0 * GRID_PADDING)
                .max(0.0);
        let columns = match tiles {
            0..=1 => 1,
            2..=4 => 2,
            5 | 6 => 3,
            // auto-fit collapses empty tracks, so a short row stretches.
            _ => {
                (((grid + GRID_GAP) / (MIN_TILE_WIDTH + GRID_GAP)).floor() as usize).clamp(1, tiles)
            }
        };
        ((grid - GRID_GAP * (columns - 1) as f64) / columns as f64).max(0.0)
    }

    /// The layer a tile asks for on its first measurement at `tiles` tiles.
    #[cfg(test)]
    pub fn spatial_layer(&self, tiles: usize) -> u8 {
        layer_for_width(self.tile_width(tiles), self.pixel_ratio, None)
    }
}

/// Opus as Chrome sends a microphone: about 32 kbit/s of speech in 20 ms
/// frames. The web client enables DTX (`opusDtx: true`), so a silent
/// microphone sends one comfort-noise frame per 400 ms instead of fifty
/// frames a second.
pub const SPEECH_BITRATE_BPS: u32 = 32_000;
pub const AUDIO_FRAMES_PER_SECOND: u32 = 50;
pub const DTX_FRAME_INTERVAL: u32 = 20;
pub const DTX_PAYLOAD_BYTES: usize = 1;

pub const fn speech_payload_bytes() -> usize {
    (SPEECH_BITRATE_BPS / 8 / AUDIO_FRAMES_PER_SECOND) as usize
}

/// Who is talking: `speakers` of a room's publishers at a time, rotating
/// every `slot`, everyone else silent behind DTX. Rotation keeps every
/// publisher's audio path exercised without changing the room's load.
#[derive(Debug, Clone, Copy)]
pub struct Speaking {
    pub index: usize,
    pub publishers: usize,
    pub speakers: usize,
    pub epoch: Instant,
    pub slot: Duration,
}

pub const SPEAKER_SLOT: Duration = Duration::from_secs(10);

impl Speaking {
    pub fn is_speaking(&self, now: Instant) -> bool {
        let slot_ms = self.slot.as_millis().max(1);
        let slot = (now.saturating_duration_since(self.epoch).as_millis() / slot_ms) as usize;
        speaks_in_slot(self.index, self.publishers, self.speakers, slot)
    }
}

/// Whether the publisher at `index` among `publishers` talks in rotation
/// `slot`, with `speakers` of them talking at once.
pub fn speaks_in_slot(index: usize, publishers: usize, speakers: usize, slot: usize) -> bool {
    if publishers == 0 || speakers == 0 || index >= publishers {
        return false;
    }
    if speakers >= publishers {
        return true;
    }
    let first = slot % publishers;
    (index + publishers - first) % publishers < speakers
}

/// Whole seconds of the first `window_seconds` after the epoch in which the
/// publisher talks, in `SPEAKER_SLOT` rotations: what validation expects of it.
pub fn speaking_seconds(
    index: usize,
    publishers: usize,
    speakers: usize,
    window_seconds: usize,
) -> usize {
    let slot = usize::try_from(SPEAKER_SLOT.as_secs())
        .unwrap_or(usize::MAX)
        .max(1);
    (0..window_seconds)
        .filter(|second| speaks_in_slot(index, publishers, speakers, second / slot))
        .count()
}

/// Decides, 20 ms frame by 20 ms frame, whether a DTX microphone sends and
/// how large the payload is.
#[derive(Debug, Default)]
pub struct DtxAudio {
    silent_frames: u32,
}

impl DtxAudio {
    /// Payload size of this frame's packet, or `None` when DTX sends nothing.
    pub fn next_frame(&mut self, speaking: bool) -> Option<usize> {
        if speaking {
            self.silent_frames = 0;
            return Some(speech_payload_bytes());
        }
        // The first silent frame is sent, then one every 400 ms.
        let send = self.silent_frames.is_multiple_of(DTX_FRAME_INTERVAL);
        self.silent_frames = self.silent_frames.wrapping_add(1);
        send.then_some(DTX_PAYLOAD_BYTES)
    }
}

/// The browser workload's settings, echoed into the run's configuration.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    pub capture: Capture,
    pub speakers_per_room: usize,
    pub viewer: Viewer,
    pub layers: [SimulcastLayer; 3],
}

impl BrowserProfile {
    pub fn new(capture: Capture, speakers_per_room: usize, viewer: Viewer) -> Self {
        Self {
            capture,
            speakers_per_room,
            viewer,
            layers: camera_layers(capture),
        }
    }
}

/// The server's default per-participant consumer cap.
pub const MAX_CONSUMERS_PER_PARTICIPANT: usize = 64;

/// A browser's (audio, video) consumer caps: it consumes every remote producer
/// until the server refuses, so half of the cap goes to each kind when rooms
/// publish both and all of it to the one kind they publish otherwise.
pub fn consumer_caps(audio: bool, video: bool) -> (usize, usize) {
    match (audio, video) {
        (true, true) => (
            MAX_CONSUMERS_PER_PARTICIPANT / 2,
            MAX_CONSUMERS_PER_PARTICIPANT / 2,
        ),
        (true, false) => (MAX_CONSUMERS_PER_PARTICIPANT, 0),
        (false, _) => (0, MAX_CONSUMERS_PER_PARTICIPANT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_choice_matches_the_web_clients_shared_cases() {
        let shared: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/tests/layer-cap-cases.json"
        )))
        .unwrap();
        let widths: Vec<f64> = shared["layerWidths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|width| width.as_f64().unwrap())
            .collect();
        assert_eq!(widths, LAYER_WIDTHS.to_vec());
        for case in shared["cases"].as_array().unwrap() {
            let current = case["current"].as_u64().map(|layer| layer as u8);
            assert_eq!(
                u64::from(layer_for_width(
                    case["renderedWidth"].as_f64().unwrap(),
                    case["pixelRatio"].as_f64().unwrap(),
                    current,
                )),
                case["layer"].as_u64().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn early_tiles_keep_their_layer_as_a_meeting_fills() {
        // A publisher on the laptop sees four peers arrive one at a time: the
        // first three tiles start on the top layer at two to four tiles and keep
        // it at five (289 CSS px still needs more than 85 % of 640 device px);
        // only the fifth tile starts fresh on the middle layer.
        let mut grid = BrowserGrid::new(laptop(), true);
        let mut requests = Vec::new();
        for peer in ["a", "b", "c", "d"] {
            grid.announce(&format!("{peer}-audio"), peer);
            grid.announce(&format!("{peer}-video"), peer);
            requests.push(grid.consume(&[
                (format!("{peer}-audio-consumer"), format!("{peer}-audio"), false),
                (format!("{peer}-video-consumer"), format!("{peer}-video"), true),
            ]));
        }
        assert_eq!(
            requests,
            vec![
                vec![("a-video-consumer".to_string(), 2)],
                vec![("b-video-consumer".to_string(), 2)],
                vec![("c-video-consumer".to_string(), 2)],
                vec![("d-video-consumer".to_string(), 1)],
            ]
        );
        // A late joiner lays out all five tiles at once: every tile starts fresh.
        let mut late = BrowserGrid::new(laptop(), true);
        let mut batch = Vec::new();
        for peer in ["a", "b", "c", "d"] {
            late.announce(&format!("{peer}-video"), peer);
            batch.push((format!("{peer}-video-consumer"), format!("{peer}-video"), true));
        }
        assert!(late.consume(&batch).iter().all(|(_, layer)| *layer == 1));
    }

    #[test]
    fn a_grid_counts_only_participants_it_consumes() {
        // One audio and one video subscription among eight publishers: the
        // viewer's grid holds two tiles, not eight, so it asks for the top layer.
        let mut grid = BrowserGrid::new(laptop(), false);
        for peer in 0..8 {
            grid.announce(&format!("{peer}-audio"), &peer.to_string());
            grid.announce(&format!("{peer}-video"), &peer.to_string());
        }
        let requests = grid.consume(&[
            ("audio-consumer".to_string(), "0-audio".to_string(), false),
            ("video-consumer".to_string(), "1-video".to_string(), true),
        ]);
        assert_eq!(requests, vec![("video-consumer".to_string(), 2)]);
    }

    #[test]
    fn speaking_seconds_follow_the_rotation() {
        // Two publishers who both talk: always.
        assert_eq!(speaking_seconds(0, 2, 2, 30), 30);
        // Three publishers, one talker in 10 s slots: each talks one slot in three.
        for index in 0..3 {
            assert_eq!(speaking_seconds(index, 3, 1, 30), 10);
        }
        assert_eq!(speaking_seconds(0, 3, 1, 25), 10);
        assert_eq!(speaking_seconds(2, 3, 1, 25), 5);
        // Nobody talks, or the index is not a publisher.
        assert_eq!(speaking_seconds(0, 3, 0, 30), 0);
        assert_eq!(speaking_seconds(3, 3, 1, 30), 0);
    }

    #[test]
    fn browsers_spend_the_consumer_cap_on_the_media_rooms_publish() {
        assert_eq!(consumer_caps(true, true), (32, 32));
        assert_eq!(consumer_caps(true, false), (64, 0));
        assert_eq!(consumer_caps(false, true), (0, 64));
    }

    fn laptop() -> Viewer {
        Viewer {
            viewport_width: 1440,
            pixel_ratio: 2.0,
            layout: Layout::Classic,
        }
    }

    #[test]
    fn camera_layers_match_the_web_client_encodings() {
        let hd = camera_layers(Capture::Hd);
        assert_eq!(
            hd.map(|layer| (layer.rid, layer.width, layer.height, layer.max_bitrate_bps)),
            [
                ("r0", 320, 180, 100_000),
                ("r1", 640, 360, 300_000),
                ("r2", 1280, 720, 900_000)
            ]
        );
        let full = camera_layers(Capture::FullHd);
        assert_eq!(full[2].max_bitrate_bps, 2_500_000);
        assert_eq!(
            (full[0].width, full[1].width, full[2].width),
            (480, 960, 1920)
        );
        assert!(Capture::parse("360p").is_err());
    }

    #[test]
    fn classic_laptop_tiles_follow_the_grid_css() {
        let viewer = laptop();
        // 1440 - 540 side panels - 16 padding = 884 px of grid.
        assert_eq!(viewer.tile_width(1), 884.0);
        assert_eq!(viewer.tile_width(2), 438.0);
        assert_eq!(viewer.tile_width(4), 438.0);
        assert_eq!(viewer.tile_width(5), (884.0 - 16.0) / 3.0);
        // auto-fit: (884 + 8) / 288 = 3 columns of at least 280 px.
        assert_eq!(viewer.tile_width(7), (884.0 - 16.0) / 3.0);
        assert_eq!(viewer.tile_width(80), (884.0 - 16.0) / 3.0);
    }

    #[test]
    fn auto_fit_stretches_a_short_row_on_a_wide_screen() {
        let wide = Viewer {
            viewport_width: 3840,
            pixel_ratio: 1.0,
            layout: Layout::Modern,
        };
        // 3840 - 320 - 16 = 3504 px fits 12 columns, but 7 tiles use 7.
        assert_eq!(wide.tile_width(7), (3504.0 - 48.0) / 7.0);
        assert_eq!(wide.tile_width(30), (3504.0 - 88.0) / 12.0);
    }

    #[test]
    fn layer_requests_match_layer_cap_for_common_screens() {
        let viewer = laptop();
        // 438 px at 2x needs 876 device px: the top layer.
        assert_eq!(viewer.spatial_layer(2), 2);
        assert_eq!(viewer.spatial_layer(4), 2);
        // 289 px at 2x needs 578: the middle layer.
        assert_eq!(viewer.spatial_layer(5), 1);
        assert_eq!(viewer.spatial_layer(30), 1);
        let desktop = Viewer {
            viewport_width: 1920,
            pixel_ratio: 1.0,
            layout: Layout::Classic,
        };
        // 1920 - 540 - 16 = 1364: four columns of 335 px need layer 1.
        assert_eq!(desktop.spatial_layer(30), 1);
        assert_eq!(desktop.spatial_layer(3), 2);
        let modern = Viewer {
            viewport_width: 1920,
            pixel_ratio: 1.0,
            layout: Layout::Modern,
        };
        // 1920 - 320 - 16 = 1584: five columns of 310 px fit the lowest layer.
        assert_eq!(modern.spatial_layer(30), 0);
        let small = Viewer {
            viewport_width: 1280,
            pixel_ratio: 1.0,
            layout: Layout::Classic,
        };
        // 1280 - 540 - 16 = 724: two columns of 358 px need layer 1.
        assert_eq!(small.spatial_layer(30), 1);
    }

    #[test]
    fn one_speaker_rotates_through_every_publisher() {
        let epoch = Instant::now();
        let at = |index, seconds| {
            Speaking {
                index,
                publishers: 4,
                speakers: 1,
                epoch,
                slot: SPEAKER_SLOT,
            }
            .is_speaking(epoch + Duration::from_secs(seconds))
        };
        assert!(at(0, 0) && !at(1, 0) && !at(3, 9));
        assert!(at(1, 10) && !at(0, 10));
        assert!(at(3, 30) && at(0, 40));
        for seconds in [0, 15, 37, 1_000] {
            assert_eq!((0..4).filter(|index| at(*index, seconds)).count(), 1);
        }
    }

    #[test]
    fn speaking_handles_empty_and_saturated_rooms() {
        let epoch = Instant::now();
        let speaking = |index, publishers, speakers| Speaking {
            index,
            publishers,
            speakers,
            epoch,
            slot: SPEAKER_SLOT,
        };
        assert!(!speaking(0, 0, 1).is_speaking(epoch));
        assert!(!speaking(0, 3, 0).is_speaking(epoch));
        assert!(speaking(2, 3, 5).is_speaking(epoch));
        assert!(!speaking(4, 3, 1).is_speaking(epoch));
        // Before the epoch (during the ramp) the first slot applies.
        let early = speaking(0, 3, 1);
        assert!(early.is_speaking(epoch - Duration::from_secs(5)));
    }

    #[test]
    fn dtx_sends_speech_every_frame_and_silence_every_400_ms() {
        let mut audio = DtxAudio::default();
        assert_eq!(audio.next_frame(true), Some(80));
        let silent: Vec<_> = (0..45).map(|_| audio.next_frame(false)).collect();
        let sent: Vec<_> = silent
            .iter()
            .enumerate()
            .filter_map(|(frame, payload)| payload.map(|size| (frame, size)))
            .collect();
        assert_eq!(sent, vec![(0, 1), (20, 1), (40, 1)]);
        // Speech resets the silence schedule.
        assert_eq!(audio.next_frame(true), Some(80));
        assert_eq!(audio.next_frame(false), Some(1));
        assert_eq!(audio.next_frame(false), None);
    }
}
