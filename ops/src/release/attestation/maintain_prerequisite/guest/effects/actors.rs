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
pub(super) struct CaptureFailure {
    first: Option<(Error, CaptureStage)>,
    owns_primary: bool,
}
impl CaptureFailure {
    pub fn observe<T>(&mut self, stage: CaptureStage, result: Result<T>) -> Result<T> {
        if let Err(error) = result {
            let stage = match error {
                Error::Bounds => CaptureStage::Bounds,
                Error::Close => CaptureStage::Close,
                _ => stage,
            };
            self.first.get_or_insert((error, stage));
        }
        result
    }
    fn result(&self) -> Result<()> {
        self.first.map_or(Ok(()), |(error, _)| Err(error))
    }
    pub fn diagnostic(&self) -> Option<(Error, &'static str)> {
        self.first.map(|(error, stage)| (error, stage.checkpoint()))
    }
    fn primary_available(control: &Control) -> bool {
        control.first.is_none() && control.status == 0 && !control.cleanup_failed
    }
    fn observe_control<T>(
        &mut self,
        available_before: bool,
        stage: CaptureStage,
        result: Result<T>,
        control: &mut Control,
    ) -> Result<T> {
        let first_capture = self.first.is_none();
        let observed = self.observe(stage, result);
        if let Err(error) = observed {
            // Control has one mutable borrower: an intervening recorded error
            // belongs to this checked watch operation. Signals/deadlines still
            // retain their original checkpoint through their terminal status.
            if first_capture && available_before && control.status == 0 {
                self.owns_primary = true;
                control.checkpoint = self.first.map_or("capture-live", |(_, s)| s.checkpoint());
            }
            control.record(error);
        }
        observed
    }
    fn live_result(
        &mut self,
        available_before: bool,
        stage: CaptureStage,
        result: Result<()>,
        control: &mut Control,
    ) -> Result<()> {
        let observed = self.observe_control(available_before, stage, result, control);
        if control.is_recovering() {
            // The transaction has already left its body. Keep pumping and
            // recording capture faults without vetoing known-role retirement;
            // only the actual original control fence may prevent those calls.
            control.check()
        } else {
            observed.and(self.result())
        }
    }
    pub fn primary_checkpoint(&self) -> Option<&'static str> {
        if self.owns_primary {
            self.diagnostic().map(|(_, checkpoint)| checkpoint)
        } else {
            None
        }
    }
}

fn live_capture(
    out: &[u8],
    err: &[u8],
    eof: [bool; 2],
    running: impl FnOnce() -> Result<bool>,
    control: &mut Control,
    stage: &mut CaptureStage,
) -> Result<()> {
    *stage = CaptureStage::Bounds;
    require(out.len() <= 1024 * 1024 && err.len() <= 8192)?;
    *stage = CaptureStage::Eof;
    require(!eof[0] && !eof[1])?;
    *stage = CaptureStage::Live;
    require(running()?)?;
    control.check()?;
    if out.len() > 24 {
        *stage = CaptureStage::Data(first_packet_class(out));
        return Err(Error::Refused);
    }
    Ok(())
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
        if !control.is_recovering() {
            self.failure.result()?;
        }
        let available = CaptureFailure::primary_available(control);
        let mut stage = CaptureStage::Live;
        let result = (|| {
            control.check()?;
            require(!self.closed)?;
            let chunk = self.actor.pump(control)?;
            let (out, err) = self.actor.snapshot();
            live_capture(
                out,
                err,
                [chunk.stdout_eof, chunk.stderr_eof],
                || self.actor.running(),
                control,
                &mut stage,
            )
        })();
        self.failure.live_result(available, stage, result, control)
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
        let available = CaptureFailure::primary_available(control);
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
        let _ = self
            .failure
            .observe_control(available, stage, result, control);
        let available = CaptureFailure::primary_available(control);
        let closed = self.actor.finish(control).await.map(|_| ());
        let _ = self
            .failure
            .observe_control(available, CaptureStage::Close, closed, control);
        self.failure.result()
    }
    pub fn primary_checkpoint(&self) -> Option<&'static str> {
        self.failure.primary_checkpoint()
    }
}

#[cfg(test)]
mod live_capture_tests {
    use super::*;

    fn packet() -> Vec<u8> {
        let mut bytes = vec![0; 24];
        bytes[..4].copy_from_slice(&[0xd4, 0xc3, 0xb2, 0xa1]);
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&4_u16.to_le_bytes());
        bytes[16..20].copy_from_slice(&128_u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&54_u32.to_le_bytes());
        bytes.extend_from_slice(&54_u32.to_le_bytes());
        let mut frame = [0; 54];
        frame[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&40_u16.to_be_bytes());
        frame[23] = 6;
        frame[34..36].copy_from_slice(&12345_u16.to_be_bytes());
        frame[36..38].copy_from_slice(&443_u16.to_be_bytes());
        frame[46] = 0x50;
        bytes.extend_from_slice(&frame);
        bytes
    }

    fn sample(
        failure: &mut CaptureFailure,
        control: &mut Control,
        out: &[u8],
        err: &[u8],
        eof: [bool; 2],
        running: impl FnOnce() -> Result<bool>,
    ) -> Result<()> {
        let available = CaptureFailure::primary_available(control);
        let mut stage = CaptureStage::Live;
        let result = live_capture(out, err, eof, running, control, &mut stage);
        failure.live_result(available, stage, result, control)
    }

    #[test]
    fn irreversible_packet_refuses_at_ready_and_native_boundaries_without_resetting_time() {
        let bytes = packet();
        assert_eq!(first_packet_class(&bytes), PacketClass::Ipv4Tcp);
        for checkpoint in ["ready", "native"] {
            let mut control = Control::new(Instant::now()).unwrap();
            let end = control.end();
            control.checkpoint = checkpoint;
            let mut failure = CaptureFailure::default();
            assert_eq!(
                sample(&mut failure, &mut control, &bytes, &[], [false; 2], || Ok(
                    true
                )),
                Err(Error::Refused)
            );
            assert_eq!(control.first, Some(Error::Refused));
            assert_eq!(control.checkpoint, "capture-data-ipv4-tcp");
            assert_eq!(failure.primary_checkpoint(), Some("capture-data-ipv4-tcp"));
            // A later empty snapshot cannot erase an already failed capture.
            assert_eq!(
                sample(
                    &mut failure,
                    &mut control,
                    &[0; 24],
                    &[],
                    [false; 2],
                    || Ok(true)
                ),
                Err(Error::Refused)
            );
            control.begin_recovery();
            control.begin_settling();
            assert_eq!(control.finish(), Err(Error::Refused));
            assert_eq!(control.end(), end);
        }
    }

    #[test]
    fn startup_header_prefixes_remain_live_but_record_prefixes_irreversibly_refuse() {
        for size in 0..=24 {
            let mut control = Control::new(Instant::now()).unwrap();
            let mut failure = CaptureFailure::default();
            assert_eq!(
                sample(
                    &mut failure,
                    &mut control,
                    &vec![0; size],
                    &[],
                    [false; 2],
                    || { Ok(true) }
                ),
                Ok(())
            );
            assert_eq!(control.first, None);
            assert_eq!(failure.primary_checkpoint(), None);
            assert_eq!(control.finish(), Ok(()));
        }
        let mut control = Control::new(Instant::now()).unwrap();
        let mut failure = CaptureFailure::default();
        assert_eq!(
            sample(
                &mut failure,
                &mut control,
                &[0; 25],
                &[],
                [false; 2],
                || Ok(true)
            ),
            Err(Error::Refused)
        );
        assert_eq!(failure.primary_checkpoint(), Some("capture-data-unknown"));
        assert_eq!(capture_output(0, &[0; 23], &[]), Some(CaptureStage::Short));
        assert_eq!(capture_output(0, &[0; 24], &[]), None);
        assert_eq!(capture_output(1, &[0; 24], &[]), Some(CaptureStage::Status));
        assert_eq!(control.finish(), Err(Error::Refused));
    }

    #[test]
    fn bounds_eof_running_and_actual_clock_fences_keep_precedence_over_packet_data() {
        for (out, err, eof, running, expected) in [
            (
                vec![0; 1024 * 1024 + 1],
                vec![],
                [true; 2],
                Ok(false),
                CaptureStage::Bounds,
            ),
            (
                packet(),
                vec![0; 8193],
                [false; 2],
                Ok(true),
                CaptureStage::Bounds,
            ),
            (
                packet(),
                vec![],
                [true, false],
                Ok(false),
                CaptureStage::Eof,
            ),
            (packet(), vec![], [false; 2], Ok(false), CaptureStage::Live),
            (
                packet(),
                vec![],
                [false; 2],
                Err(Error::Close),
                CaptureStage::Close,
            ),
        ] {
            let mut control = Control::new(Instant::now()).unwrap();
            let mut failure = CaptureFailure::default();
            assert!(sample(&mut failure, &mut control, &out, &err, eof, || running).is_err());
            assert_eq!(failure.primary_checkpoint(), Some(expected.checkpoint()));
            assert!(control.finish().is_err());
        }
        let mut control = Control::new(Instant::now()).unwrap();
        control.checkpoint = "native";
        let end = control.end();
        assert_eq!(control.push_phase(Duration::ZERO), Err(Error::Deadline));
        let mut failure = CaptureFailure::default();
        assert_eq!(
            sample(
                &mut failure,
                &mut control,
                &packet(),
                &[],
                [false; 2],
                || Ok(true)
            ),
            Err(Error::Deadline)
        );
        assert_eq!(failure.primary_checkpoint(), None);
        assert_eq!(control.checkpoint, "native");
        assert_eq!(control.first, Some(Error::Deadline));
        control.begin_settling();
        assert_eq!(control.finish(), Err(Error::Deadline));
        assert_eq!(control.end(), end);
    }

    #[test]
    fn prior_body_collector_clock_and_close_causes_cannot_become_capture_primary() {
        for earlier in [
            Error::Io,
            Error::Refused,
            Error::Bounds,
            Error::Command,
            Error::Deadline,
            Error::Interrupt,
            Error::Terminate,
            Error::Close,
        ] {
            let mut control = Control::new(Instant::now()).unwrap();
            control.checkpoint = "daemon-observe";
            control.record(earlier);
            let mut failure = CaptureFailure::default();
            let _ = sample(
                &mut failure,
                &mut control,
                &packet(),
                &[],
                [false; 2],
                || Ok(true),
            );
            assert_eq!(control.first, Some(earlier));
            assert_eq!(control.checkpoint, "daemon-observe");
            assert_eq!(failure.primary_checkpoint(), None);
            control.begin_recovery();
            control.begin_settling();
            assert_eq!(control.finish(), Err(earlier));
        }
    }

    #[test]
    fn recovery_still_checks_live_snapshots_and_final_refusal_survives_later_close() {
        let mut control = Control::new(Instant::now()).unwrap();
        let end = control.end();
        let mut failure = CaptureFailure::default();
        assert!(
            sample(
                &mut failure,
                &mut control,
                &packet(),
                &[],
                [false; 2],
                || Ok(true)
            )
            .is_err()
        );
        control.begin_recovery();
        let mut inspected = false;
        assert_eq!(
            sample(
                &mut failure,
                &mut control,
                &packet(),
                &[],
                [false; 2],
                || {
                    inspected = true;
                    Ok(true)
                }
            ),
            Ok(())
        );
        assert!(inspected);
        let available = CaptureFailure::primary_available(&control);
        assert_eq!(
            failure.live_result(
                available,
                CaptureStage::Live,
                Err(Error::Close),
                &mut control
            ),
            Ok(())
        );
        assert_eq!(failure.result(), Err(Error::Refused));
        assert_eq!(failure.primary_checkpoint(), Some("capture-data-ipv4-tcp"));
        assert_eq!(control.push_phase(Duration::ZERO), Err(Error::Deadline));
        assert_eq!(
            sample(
                &mut failure,
                &mut control,
                &packet(),
                &[],
                [false; 2],
                || Ok(true)
            ),
            Err(Error::Deadline)
        );
        assert_eq!(control.status, 124);
        control.begin_settling();
        assert_eq!(control.finish(), Err(Error::Refused));
        assert_eq!(control.end(), end);
    }

    #[test]
    fn first_watch_failure_during_cleanup_replaces_the_saved_success_checkpoint_only() {
        for earlier in [None, Some(Error::Refused)] {
            let mut control = Control::new(Instant::now()).unwrap();
            control.checkpoint = "source-observe";
            if let Some(error) = earlier {
                control.record(error);
            }
            let saved = control.checkpoint;
            control.begin_recovery();
            let mut failure = CaptureFailure::default();
            assert_eq!(
                sample(
                    &mut failure,
                    &mut control,
                    &packet(),
                    &[],
                    [false; 2],
                    || Ok(true)
                ),
                Ok(())
            );
            let checkpoint =
                super::super::super::local_checkpoint(saved, failure.primary_checkpoint());
            assert_eq!(
                checkpoint,
                if earlier.is_none() {
                    "capture-data-ipv4-tcp"
                } else {
                    saved
                }
            );
            assert_eq!(failure.result(), Err(Error::Refused));
            control.begin_settling();
            assert_eq!(control.finish(), Err(Error::Refused));
        }
    }

    #[test]
    fn watch_owned_close_uses_before_operation_provenance_not_matching_error_values() {
        let mut control = Control::new(Instant::now()).unwrap();
        let mut failure = CaptureFailure::default();
        let available = CaptureFailure::primary_available(&control);
        // A checked watch owner may record its close error before returning it.
        control.cleanup_failed = true;
        control.record(Error::Close);
        assert_eq!(
            failure.observe_control(
                available,
                CaptureStage::Close,
                Err::<(), _>(Error::Close),
                &mut control
            ),
            Err(Error::Close)
        );
        assert_eq!(failure.primary_checkpoint(), Some("capture-close"));
        control.begin_settling();
        assert_eq!(control.finish(), Err(Error::Close));
    }
}
