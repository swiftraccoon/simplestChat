//! The two live, bounded observers owned by this one disposable guest.

use super::super::{Context, Control, Error, Result, process::Running};
use super::{policy, policy::require};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) type Shared = Arc<Mutex<Monitors>>;

pub(super) fn append_events(
    rows: &mut Vec<Value>,
    buffer: &mut Vec<u8>,
    consumed: &mut usize,
    bytes: &[u8],
) -> Result<()> {
    *consumed = consumed.checked_add(bytes.len()).ok_or(Error::Bounds)?;
    require(*consumed <= 2 * 1024 * 1024)?;
    buffer.extend_from_slice(bytes);
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
        require(end > 0 && end <= 131072 && rows.len() < 2048)?;
        let row = policy::decode(&buffer[..end])?;
        policy::object(&row)?;
        rows.push(row);
        buffer.drain(..=end);
    }
    require(buffer.len() <= 131072)
}

#[derive(Default)]
pub(super) struct Monitors {
    pub collector: Option<Collector>,
    pub watch: Option<Watch>,
}
impl Monitors {
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        if let Some(collector) = &mut self.collector
            && !collector.closed
        {
            collector.check(control)?;
        }
        if let Some(watch) = &mut self.watch
            && !watch.closed
        {
            watch.check(control)?;
        }
        Ok(())
    }
}
pub(super) fn install(ctx: &mut Context, monitors: &Shared) {
    let shared = monitors.clone();
    ctx.commands.set_monitor(Some(Box::new(move |control| {
        shared.lock().map_err(|_| Error::Io)?.check(control)
    })));
}

pub(super) struct Collector {
    actor: Running,
    pub rows: Vec<Value>,
    buffer: Vec<u8>,
    consumed: usize,
    pub marker: Option<String>,
    pub native_start: usize,
    pub closed: bool,
    failure: Option<Error>,
}
impl Collector {
    pub async fn start(ctx: &mut Context) -> Result<Self> {
        ctx.check()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Io)?;
        let since = format!("{}.{:09}", now.as_secs(), now.subsec_nanos());
        let args = [
            "docker".into(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "events".into(),
            "--since".into(),
            since,
            "--format".into(),
            "{{json .}}".into(),
        ];
        let actor = ctx.commands.spawn(
            &mut ctx.control,
            "daemon-events",
            &args,
            Duration::from_secs(1800),
        )?;
        let mut value = Self {
            actor,
            rows: Vec::new(),
            buffer: Vec::new(),
            consumed: 0,
            marker: None,
            native_start: 0,
            closed: false,
            failure: None,
        };
        let configured = (|| {
            value.actor.set_retirement_grace(Duration::from_secs(1))?;
            value.actor.set_limits(2 * 1024 * 1024, 2 * 1024 * 1024)?;
            ctx.control.check()
        })();
        if let Err(error) = configured {
            ctx.control.record(error);
            if let Err(later) = value.actor.finish(&mut ctx.control).await {
                ctx.control.record(later);
            }
            return Err(error);
        }
        Ok(value)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        append_events(&mut self.rows, &mut self.buffer, &mut self.consumed, bytes)
    }
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            control.check()?;
            require(!self.closed)?;
            let chunk = self.actor.pump(control)?;
            self.append(&chunk.stdout)?;
            require(!chunk.stdout_eof && !chunk.stderr_eof && self.actor.running()?)?;
            control.check()
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
        }
        result
    }
    pub fn boundary(&mut self, control: &mut Control) -> Result<()> {
        require(self.marker.is_some())?;
        self.check(control)?;
        self.native_start = self.rows.len();
        Ok(())
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<()> {
        if self.closed {
            return self.failure.map_or(Ok(()), Err);
        }
        self.closed = true;
        let mut first = self.failure;
        let result = async {
            control.check()?;
            let end = Instant::now()
                .checked_add(control.remaining(Duration::from_secs(15))?)
                .ok_or(Error::Deadline)?;
            if self.actor.running()? {
                self.actor.signal(control, rustix::process::Signal::INT)?;
            }
            let allowance = end
                .saturating_duration_since(Instant::now())
                .checked_sub(Duration::from_secs(6))
                .ok_or(Error::Deadline)?;
            let observed = self.actor.wait(control, allowance).await?;
            require(observed.stdout.len() >= self.consumed)?;
            let consumed = self.consumed;
            self.append(&observed.stdout[consumed..])?;
            require(self.buffer.is_empty() && [0, -2, 130].contains(&observed.status))?;
            control.check()
        }
        .await;
        if let Err(error) = result {
            first.get_or_insert(error);
            control.record(error);
        }
        if let Err(error) = self.actor.finish(control).await {
            first.get_or_insert(error);
            control.record(error);
        }
        self.failure = first;
        first.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PacketClass {
    Arp,
    Ipv4,
    Ipv4Icmp,
    Ipv4Igmp,
    Ipv4Tcp,
    Ipv4Udp,
    Ipv4Dhcp,
    Ipv4Ntp,
    Ipv4Dns,
    Ipv6,
    Icmpv6RouterAdvertisement,
    Other,
    Unknown,
}

fn pcap_u16(bytes: &[u8], offset: usize, little: bool) -> Option<u16> {
    let word = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if little {
        u16::from_le_bytes(word)
    } else {
        u16::from_be_bytes(word)
    })
}
fn pcap_u32(bytes: &[u8], offset: usize, little: bool) -> Option<u32> {
    let word = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(if little {
        u32::from_le_bytes(word)
    } else {
        u32::from_be_bytes(word)
    })
}

/// Refine only complete first IPv4/transport headers, never payload validity.
fn ipv4_header_class(frame: &[u8], original: u32) -> PacketClass {
    let observed = (|| {
        let ip = frame.get(14..)?;
        let version = *ip.first()?;
        let ihl = usize::from(version & 15) * 4;
        if version >> 4 != 4 || !(20..=60).contains(&ihl) {
            return None;
        }
        ip.get(..ihl)?;
        let total = usize::from(pcap_u16(ip, 2, false)?);
        if total < ihl
            || total.checked_add(14)? > usize::try_from(original).ok()?
            || pcap_u16(ip, 6, false)? & !0x4000 != 0
        {
            return None;
        }
        let transport = ip.get(ihl..)?;
        let minimum = match *ip.get(9)? {
            1 | 2 | 17 => 8,
            6 => 20,
            _ => return None,
        };
        if total < ihl.checked_add(minimum)? {
            return None;
        }
        transport.get(..minimum)?;
        match ip[9] {
            1 => Some(PacketClass::Ipv4Icmp),
            2 => Some(PacketClass::Ipv4Igmp),
            6 => {
                let tcp_header = usize::from(transport[12] >> 4) * 4;
                if !(20..=60).contains(&tcp_header) || total < ihl + tcp_header {
                    return None;
                }
                transport.get(..tcp_header)?;
                let source = pcap_u16(transport, 0, false)?;
                let destination = pcap_u16(transport, 2, false)?;
                Some(if [source, destination].contains(&53) {
                    PacketClass::Ipv4Dns
                } else {
                    PacketClass::Ipv4Tcp
                })
            }
            17 => {
                let udp_length = usize::from(pcap_u16(transport, 4, false)?);
                if udp_length < 8 || udp_length > total - ihl {
                    return None;
                }
                let source = pcap_u16(transport, 0, false)?;
                let destination = pcap_u16(transport, 2, false)?;
                let dns = [source, destination].contains(&53);
                let ntp = [source, destination].contains(&123);
                if dns && ntp {
                    return None;
                }
                Some(
                    if [source, destination] == [67, 68] || [source, destination] == [68, 67] {
                        PacketClass::Ipv4Dhcp
                    } else if ntp {
                        PacketClass::Ipv4Ntp
                    } else if dns {
                        PacketClass::Ipv4Dns
                    } else {
                        PacketClass::Ipv4Udp
                    },
                )
            }
            _ => None,
        }
    })();
    observed.unwrap_or(PacketClass::Ipv4)
}

/// Best-effort first Ethernet-header class, never packet validity or origin.
///
/// Only the first bounded classic-PCAP record is inspected. Addresses, times,
/// later records and direction are neither returned nor validated. Every data
/// class remains a refusal under the unchanged 24-byte empty-capture rule.
pub(super) fn first_packet_class(bytes: &[u8]) -> PacketClass {
    let observed = (|| {
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        let little = match bytes.get(..4)? {
            [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
            [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
            _ => return None,
        };
        let captured = usize::try_from(pcap_u32(bytes, 32, little)?).ok()?;
        let original = pcap_u32(bytes, 36, little)?;
        if pcap_u16(bytes, 4, little)? != 2
            || pcap_u16(bytes, 6, little)? != 4
            || pcap_u32(bytes, 16, little)? != 128
            || pcap_u32(bytes, 20, little)? != 1
            || !(14..=128).contains(&captured)
            || u32::try_from(captured).ok()? > original
        {
            return None;
        }
        let frame = bytes.get(40..40usize.checked_add(captured)?)?;
        Some(match frame.get(12..14)? {
            [0x08, 0x06] => PacketClass::Arp,
            [0x08, 0x00] => ipv4_header_class(frame, original),
            [0x86, 0xdd] => {
                if frame.get(14).is_some_and(|v| v >> 4 == 6)
                    && frame.get(20) == Some(&58)
                    && pcap_u16(frame, 18, false).is_some_and(|n| n >= 16)
                    && frame.get(54..70).is_some_and(|icmp| icmp[..2] == [134, 0])
                {
                    PacketClass::Icmpv6RouterAdvertisement
                } else {
                    PacketClass::Ipv6
                }
            }
            _ => PacketClass::Other,
        })
    })();
    observed.unwrap_or(PacketClass::Unknown)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptureStage {
    Live,
    Bounds,
    Eof,
    Stop,
    Wait,
    Status,
    Short,
    Data(PacketClass),
    Close,
}
impl CaptureStage {
    pub fn checkpoint(self) -> &'static str {
        match self {
            Self::Live => "capture-live",
            Self::Bounds => "capture-bounds",
            Self::Eof => "capture-eof",
            Self::Stop => "capture-stop",
            Self::Wait => "capture-wait",
            Self::Status => "capture-status",
            Self::Short => "capture-short",
            Self::Data(PacketClass::Arp) => "capture-data-arp",
            Self::Data(PacketClass::Ipv4) => "capture-data-ipv4",
            Self::Data(PacketClass::Ipv4Icmp) => "capture-data-ipv4-icmp",
            Self::Data(PacketClass::Ipv4Igmp) => "capture-data-ipv4-igmp",
            Self::Data(PacketClass::Ipv4Tcp) => "capture-data-ipv4-tcp",
            Self::Data(PacketClass::Ipv4Udp) => "capture-data-ipv4-udp",
            Self::Data(PacketClass::Ipv4Dhcp) => "capture-data-ipv4-dhcp",
            Self::Data(PacketClass::Ipv4Ntp) => "capture-data-ipv4-ntp",
            Self::Data(PacketClass::Ipv4Dns) => "capture-data-ipv4-dns",
            Self::Data(PacketClass::Ipv6) => "capture-data-ipv6",
            Self::Data(PacketClass::Icmpv6RouterAdvertisement) => {
                "capture-data-icmpv6-router-advertisement"
            }
            Self::Data(PacketClass::Other) => "capture-data-other",
            Self::Data(PacketClass::Unknown) => "capture-data-unknown",
            Self::Close => "capture-close",
        }
    }
}

#[derive(Default)]
pub(super) struct CaptureFailure(Option<(Error, CaptureStage)>);
impl CaptureFailure {
    pub fn observe<T>(&mut self, stage: CaptureStage, result: Result<T>) -> Result<T> {
        if let Err(error) = result {
            let stage = match error {
                Error::Bounds => CaptureStage::Bounds,
                Error::Close => CaptureStage::Close,
                _ => stage,
            };
            self.0.get_or_insert((error, stage));
        }
        result
    }
    fn result(&self) -> Result<()> {
        self.0.map_or(Ok(()), |(error, _)| Err(error))
    }
    pub fn diagnostic(&self) -> Option<(Error, &'static str)> {
        self.0.map(|(error, stage)| (error, stage.checkpoint()))
    }
}

pub(super) fn capture_output(status: i32, out: &[u8], err: &[u8]) -> Option<CaptureStage> {
    if status != 0 {
        Some(CaptureStage::Status)
    } else if out.len() > 1024 * 1024 || err.len() > 8192 {
        Some(CaptureStage::Bounds)
    } else if out.len() < 24 {
        Some(CaptureStage::Short)
    } else if out.len() > 24 {
        Some(CaptureStage::Data(first_packet_class(out)))
    } else {
        None
    }
}

pub(super) struct Watch {
    actor: Running,
    pub closed: bool,
    failure: CaptureFailure,
}
impl Watch {
    pub async fn start(ctx: &mut Context, interface: &str) -> Result<Self> {
        let args = [
            "tcpdump".into(),
            "-p".into(),
            "-n".into(),
            "-U".into(),
            "-i".into(),
            interface.into(),
            "-s".into(),
            "128".into(),
            "-c".into(),
            "4096".into(),
            "-w".into(),
            "-".into(),
            "not (ip and host 10.0.2.2 and tcp port 22) and not (arp and host 10.0.2.2)".into(),
        ];
        let actor = ctx.commands.spawn(
            &mut ctx.control,
            "wire-capture",
            &args,
            Duration::from_secs(1800),
        )?;
        let mut value = Self {
            actor,
            closed: false,
            failure: CaptureFailure::default(),
        };
        let configured = (|| {
            value.actor.set_retirement_grace(Duration::from_secs(1))?;
            value.actor.set_limits(1024 * 1024, 8192)?;
            ctx.control.check()
        })();
        if let Err(error) = configured {
            ctx.control.record(error);
            if let Err(later) = value.actor.finish(&mut ctx.control).await {
                ctx.control.record(later);
            }
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        self.failure.result()?;
        let mut stage = CaptureStage::Live;
        let result = (|| {
            control.check()?;
            require(!self.closed)?;
            let chunk = self.actor.pump(control)?;
            let (out, err) = self.actor.snapshot();
            stage = CaptureStage::Bounds;
            require(out.len() <= 1024 * 1024 && err.len() <= 8192)?;
            stage = CaptureStage::Eof;
            require(!chunk.stdout_eof && !chunk.stderr_eof)?;
            stage = CaptureStage::Live;
            require(self.actor.running()?)?;
            control.check()
        })();
        self.failure.observe(stage, result)
    }
    pub fn ready(&self) -> bool {
        self.actor
            .snapshot()
            .1
            .windows(b"listening on".len())
            .any(|v| v == b"listening on")
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<()> {
        if self.closed {
            return self.failure.result();
        }
        if let Err(error) = self.check(control) {
            control.record(error);
        }
        self.closed = true;
        let mut stage = CaptureStage::Stop;
        let result = async {
            if self.actor.running()? {
                self.actor.signal(control, rustix::process::Signal::INT)?;
            }
            stage = CaptureStage::Wait;
            let observed = self.actor.wait(control, Duration::from_secs(5)).await?;
            if let Some(failed) =
                capture_output(observed.status, &observed.stdout, &observed.stderr)
            {
                stage = failed;
                return Err(Error::Refused);
            }
            stage = CaptureStage::Live;
            control.check()
        }
        .await;
        if let Err(error) = self.failure.observe(stage, result) {
            control.record(error);
        }
        let closed = self.actor.finish(control).await.map(|_| ());
        if let Err(error) = self.failure.observe(CaptureStage::Close, closed) {
            control.record(error);
        }
        self.failure.result()
    }
    pub fn failure(&self) -> Option<(Error, &'static str)> {
        self.failure.diagnostic()
    }
}
