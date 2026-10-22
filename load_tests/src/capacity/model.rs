//! Typed workload provenance and the unchanged host sizing/resource policy.

use super::{format::general, json::Json};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

pub(crate) const MIB: f64 = 1_048_576.0;
pub(crate) const WORKLOADS: [&str; 3] = ["meetings", "large-meeting", "webinar"];
const MEMORY_HEADROOM: f64 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Profile {
    pub(crate) meeting_size: i64,
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
    pub(crate) fn read(report: &Json) -> Result<Self> {
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

    pub(crate) fn describe(&self) -> String {
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
pub(crate) struct Ceiling {
    pub(crate) size: Option<i64>,
    pub(crate) egress_mbps: f64,
    pub(crate) memory_bytes: i64,
    pub(crate) workers: i64,
}

pub(crate) type Ceilings = BTreeMap<String, Ceiling>;

pub(crate) fn reference() -> Ceilings {
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

pub(crate) fn ceilings(report: &Json) -> Result<Ceilings> {
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
pub(crate) struct Host {
    pub(crate) vcpus: i64,
    pub(crate) memory_mib: i64,
    pub(crate) port_mbps: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Deployment {
    pub(crate) cpus: f64,
    pub(crate) memory_mib: i64,
}

impl Deployment {
    pub(crate) fn workers(self) -> i64 {
        self.cpus.floor().clamp(1.0, 64.0) as i64
    }
}

#[derive(Debug)]
pub(crate) struct Projection {
    pub(crate) per_core: f64,
    pub(crate) cpu_participants: i64,
    pub(crate) memory_limit_mib: i64,
    pub(crate) participants: i64,
    pub(crate) limited_by: &'static str,
    pub(crate) mbps_per_participant: f64,
}

fn floor(value: f64) -> Result<i64> {
    ensure!(
        value.is_finite() && value >= i64::MIN as f64 && value < -(i64::MIN as f64),
        "capacity projection is outside the integer range"
    );
    Ok(value.floor() as i64)
}

pub(crate) fn project(ceilings: &Ceilings, deployment: Deployment) -> Result<Projection> {
    let Some(meetings) = ceilings
        .get("meetings")
        .filter(|value| value.size.is_some_and(|size| size != 0))
    else {
        return Ok(Projection {
            per_core: 0.0,
            cpu_participants: 0,
            memory_limit_mib: 0,
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
    let mut needed = 0.0;
    if meetings.memory_bytes != 0 {
        let per_participant = meetings.memory_bytes as f64 / size * MEMORY_HEADROOM;
        let memory_participants = floor(deployment.memory_mib as f64 * MIB / per_participant)?;
        if memory_participants < participants {
            participants = memory_participants;
            limited_by = "memory";
        }
        needed = per_participant * participants as f64;
    }
    for (name, ceiling) in ceilings {
        if name == "meetings" || ceiling.memory_bytes == 0 {
            continue;
        }
        let spread = deployment.workers() as f64 / ceiling.workers.max(1) as f64;
        let scale = if name == "webinar" {
            spread.max(1.0)
        } else {
            1.0
        };
        needed = needed.max(ceiling.memory_bytes as f64 * scale * MEMORY_HEADROOM);
    }
    let memory_limit_mib = if needed != 0.0 {
        let rounded = (needed / MIB / 256.0).ceil() * 256.0;
        floor(rounded)?.max(512).min(deployment.memory_mib)
    } else {
        0
    };
    Ok(Projection {
        per_core,
        cpu_participants,
        memory_limit_mib,
        participants,
        limited_by,
        mbps_per_participant: meetings.egress_mbps / size,
    })
}

#[derive(Debug)]
pub(crate) struct Suggestion {
    pub(crate) deployment: Deployment,
    pub(crate) meetings: i64,
    pub(crate) meeting_limit: &'static str,
    pub(crate) viewers: i64,
    pub(crate) viewer_limit: &'static str,
    pub(crate) room: i64,
    pub(crate) connections: i64,
}

impl Suggestion {
    pub(crate) fn settings(&self) -> Vec<String> {
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

/// Host resources left after the proxy, database and system reserve.
pub(crate) fn deployment(
    host_cpus: i64,
    host_mib: i64,
    cpus: Option<f64>,
    memory: Option<i64>,
) -> Result<Deployment> {
    let reserve = 1024.max((host_mib as f64 * 0.25) as i64);
    let result = Deployment {
        cpus: cpus
            .filter(|value| *value != 0.0)
            .unwrap_or((host_cpus as f64 - 1.0).max(1.0)),
        memory_mib: memory
            .filter(|value| *value != 0)
            .unwrap_or((host_mib - reserve).max(512)),
    };
    ensure!(result.cpus.is_finite(), "app CPUs must be finite");
    Ok(result)
}

pub(crate) fn suggest(
    host: Host,
    ceilings: &Ceilings,
    cpus: Option<f64>,
    memory: Option<i64>,
) -> Result<Suggestion> {
    let deployment = deployment(host.vcpus, host.memory_mib, cpus, memory)?;
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
