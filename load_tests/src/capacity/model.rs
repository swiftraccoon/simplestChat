//! Typed workload provenance and the unchanged host sizing/resource policy.

use super::{format::general, json::Json};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

pub(super) const MIB: f64 = 1_048_576.0;
pub(super) const WORKLOADS: [&str; 3] = ["meetings", "large-meeting", "webinar"];
const MEMORY_HEADROOM: f64 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Profile {
    pub(super) meeting_size: i64,
    capture: String,
    speakers: i64,
    viewport_width: i64,
    pixel_ratio: f64,
    layout: String,
    audio_only: bool,
    chat_interval_ms: i64,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            meeting_size: 5,
            capture: "720p".into(),
            speakers: 1,
            viewport_width: 1440,
            pixel_ratio: 2.0,
            layout: "classic".into(),
            audio_only: false,
            chat_interval_ms: 0,
        }
    }
}

impl Profile {
    pub(super) fn read(report: &Json) -> Result<Self> {
        let measurement = &report["measurement"];
        measurement.object("measurement")?;
        let browser = &measurement["browser"];
        browser.object("measurement.browser")?;
        let audio_only = match browser.get("audioOnly") {
            None => false,
            Some(Json::Bool(value)) => *value,
            _ => anyhow::bail!("measurement.browser.audioOnly must be a boolean"),
        };
        Ok(Self {
            meeting_size: measurement["meetingSize"].integer("meetingSize")?,
            capture: browser
                .get("capture")
                .map_or_else(String::new, Json::display),
            speakers: browser["speakers"].integer("speakers")?,
            viewport_width: browser["viewportWidth"].integer("viewportWidth")?,
            pixel_ratio: browser["pixelRatio"].number("pixelRatio")?,
            layout: browser
                .get("layout")
                .map_or_else(String::new, Json::display),
            audio_only,
            chat_interval_ms: browser
                .get("chatIntervalMs")
                .map_or(Ok(0), |value| value.integer("chatIntervalMs"))?,
        })
    }

    pub(super) fn describe(&self) -> String {
        let media = if self.audio_only {
            "audio only"
        } else {
            "audio and video"
        };
        let chat = if self.chat_interval_ms != 0 {
            format!(
                "one chat message per participant every {} s",
                general(self.chat_interval_ms as f64 / 1000.0)
            )
        } else {
            "chat disabled".into()
        };
        format!(
            "meetings of {}, {media}, up to {} concurrent speakers per room, {chat}",
            self.meeting_size, self.speakers
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct Ceiling {
    pub(super) size: Option<i64>,
    pub(super) egress_mbps: f64,
    pub(super) memory_bytes: i64,
    pub(super) workers: i64,
}

pub(super) type Ceilings = BTreeMap<String, Ceiling>;

pub(super) fn reference() -> Ceilings {
    [
        ("meetings", 85, 85.0 * 2.39, 170, 1),
        ("large-meeting", 28, 28.0 * 3.0, 84, 2),
        ("webinar", 175, 175.0, 224, 1),
    ]
    .into_iter()
    .map(|(name, size, egress_mbps, memory, workers)| {
        (
            name.into(),
            Ceiling {
                size: Some(size),
                egress_mbps,
                memory_bytes: memory * 1_048_576,
                workers,
            },
        )
    })
    .collect()
}

pub(super) fn ceilings(report: &Json) -> Result<Ceilings> {
    let entries = report["ceilings"].object("ceilings")?;
    let mut result = BTreeMap::new();
    for name in WORKLOADS {
        let Some(entry) = entries.get(name) else {
            continue;
        };
        entry.object(name)?;
        let field = |snake: &str, camel: &str| entry.get(snake).or_else(|| entry.get(camel));
        let number = |snake, camel, default, what| {
            field(snake, camel).map_or(Ok(default), |value| value.number(what))
        };
        number("busiest_worker", "busiestWorker", 0.0, "busiest_worker")?;
        let size = match entry.get("ceiling") {
            None | Some(Json::Null) => None,
            Some(value) => Some(value.integer("ceiling")?),
        };
        result.insert(
            name.into(),
            Ceiling {
                size,
                egress_mbps: number("egress_mbps", "egressMbps", 0.0, "egress_mbps")?,
                memory_bytes: field("memory_peak_bytes", "memoryPeakBytes")
                    .map_or(Ok(0), |value| value.integer("memory_peak_bytes"))?,
                workers: entry
                    .get("workers")
                    .map_or(Ok(1), |value| value.integer("workers"))?,
            },
        );
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Host {
    pub(super) vcpus: i64,
    pub(super) memory_mib: i64,
    pub(super) port_mbps: f64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Deployment {
    pub(super) cpus: f64,
    pub(super) memory_mib: i64,
}

impl Deployment {
    pub(super) fn workers(self) -> i64 {
        self.cpus.floor().clamp(1.0, 64.0) as i64
    }
}

#[derive(Debug)]
pub(super) struct Projection {
    pub(super) participants: i64,
    pub(super) limited_by: &'static str,
    pub(super) mbps_per_participant: f64,
}

fn floor(value: f64) -> Result<i64> {
    ensure!(
        value.is_finite() && value >= i64::MIN as f64 && value < -(i64::MIN as f64),
        "capacity projection is outside the integer range"
    );
    Ok(value.floor() as i64)
}

pub(super) fn project(ceilings: &Ceilings, deployment: Deployment) -> Result<Projection> {
    let Some(meetings) = ceilings
        .get("meetings")
        .filter(|value| value.size.is_some_and(|size| size != 0))
    else {
        return Ok(Projection {
            participants: 0,
            limited_by: "cpu",
            mbps_per_participant: 0.0,
        });
    };
    let size = meetings.size.unwrap() as f64;
    let per_core = size / meetings.workers as f64;
    let cpu_participants = floor(per_core * deployment.workers() as f64)?;
    let mut participants = cpu_participants;
    let mut limited_by = "cpu";
    if meetings.memory_bytes != 0 {
        let per_participant = meetings.memory_bytes as f64 / size * MEMORY_HEADROOM;
        let memory_participants = floor(deployment.memory_mib as f64 * MIB / per_participant)?;
        if memory_participants < participants {
            participants = memory_participants;
            limited_by = "memory";
        }
    }
    Ok(Projection {
        participants,
        limited_by,
        mbps_per_participant: meetings.egress_mbps / size,
    })
}

#[derive(Debug)]
pub(super) struct Suggestion {
    pub(super) deployment: Deployment,
    pub(super) meetings: i64,
    pub(super) meeting_limit: &'static str,
    pub(super) viewers: i64,
    pub(super) viewer_limit: &'static str,
    pub(super) room: i64,
    pub(super) connections: i64,
}

impl Suggestion {
    pub(super) fn settings(&self) -> Vec<String> {
        let deployment = self.deployment;
        vec![
            format!("SIMPLESTCHAT_CPUS={}", general(deployment.cpus)),
            format!("MEDIA_WORKERS={}", deployment.workers()),
            concat!(
                "host sysctl: net.core.rmem_max=2097152 net.core.wmem_max=2097152 ",
                "(the workers ask for 1 MiB socket buffers)"
            )
            .into(),
            format!("SIMPLESTCHAT_MEMORY_LIMIT={}m", deployment.memory_mib),
            format!("MAX_CONNECTIONS={}", self.connections),
            format!("MAX_ROOMS={}", self.connections.max(64)),
            format!(
                "MAX_PARTICIPANTS_PER_ROOM={}",
                self.viewers.clamp(2, 10_000)
            ),
            "MAX_BROADCASTERS_PER_ROOM=30".into(),
            format!("RTC_PORT_END={}", 40000 + deployment.workers() - 1),
        ]
    }
}

pub(super) fn suggest(
    host: Host,
    ceilings: &Ceilings,
    cpus: Option<f64>,
    memory: Option<i64>,
) -> Result<Suggestion> {
    let reserve = 1024.max((host.memory_mib as f64 * 0.25) as i64);
    let deployment = Deployment {
        cpus: cpus
            .filter(|value| *value != 0.0)
            .unwrap_or((host.vcpus as f64 - 1.0).max(1.0)),
        memory_mib: memory
            .filter(|value| *value != 0)
            .unwrap_or((host.memory_mib - reserve).max(512)),
    };
    ensure!(deployment.cpus.is_finite(), "app CPUs must be finite");
    let projection = project(ceilings, deployment)?;
    let mut meetings = projection.participants;
    let mut meeting_limit = projection.limited_by;
    if host.port_mbps > 0.0 && projection.mbps_per_participant > 0.0 {
        let network = floor(host.port_mbps * 0.8 / projection.mbps_per_participant)?;
        if network < meetings {
            meetings = network;
            meeting_limit = "network";
        }
    }
    let mut viewers = 0;
    let mut viewer_limit = "cpu";
    if let Some(webinar) = ceilings
        .get("webinar")
        .filter(|value| value.size.is_some_and(|size| size != 0))
    {
        let size = webinar.size.unwrap() as f64;
        viewers = floor(size / webinar.workers.max(1) as f64 * deployment.workers() as f64)?;
        if webinar.memory_bytes != 0 {
            let per_viewer = webinar.memory_bytes as f64 / size * MEMORY_HEADROOM;
            let memory = floor(deployment.memory_mib as f64 * MIB / per_viewer)?;
            if memory < viewers {
                viewers = memory;
                viewer_limit = "memory";
            }
        }
        let per_viewer = webinar.egress_mbps / size;
        if host.port_mbps > 0.0 && per_viewer > 0.0 {
            let network = floor(host.port_mbps * 0.8 / per_viewer)?;
            if network < viewers {
                viewers = network;
                viewer_limit = "network";
            }
        }
    }
    let room = ceilings
        .get("large-meeting")
        .and_then(|value| value.size)
        .filter(|size| *size != 0)
        .map(|size| floor(size as f64 * 0.9).map(|size| size.max(2)))
        .transpose()?
        .unwrap_or(0);
    let connections = meetings.max(viewers).max(10);
    Ok(Suggestion {
        deployment,
        meetings,
        meeting_limit,
        viewers,
        viewer_limit,
        room,
        connections,
    })
}
