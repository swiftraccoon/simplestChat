//! Fixed recipe and conservative same-clock dispatch scheduling.
use super::{failure, io::Role};
use crate::{
    ci::{
        command::{Command, Program},
        container_engine,
    },
    tooling::Result,
};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
pub(super) const ENTRY: Duration = Duration::from_secs(2400);
pub(super) const TAIL: Duration = Duration::from_secs(1260);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(20);
pub(super) const CAPTURE: u64 = 64 * 1024;
pub(super) const LOG: u64 = 32 * 1024 * 1024;
pub(super) const RUN_KEY: &str = "simplestchat.benchmark.run";
pub(super) const ROLE_KEY: &str = "simplestchat.benchmark.role";
struct Inner {
    start: Instant,
    entry: Duration,
    reserve: Duration,
    virtual_ns: AtomicU64,
    retirement_ns: AtomicU64,
}
#[derive(Clone)]
pub(super) struct Time(Arc<Inner>);
impl Time {
    pub fn new(start: Instant) -> Self {
        Self::profile(start, ENTRY, TAIL)
    }
    pub fn recovery(start: Instant) -> Self {
        Self::profile(start, TAIL, Duration::ZERO)
    }
    fn profile(start: Instant, entry: Duration, reserve: Duration) -> Self {
        Self(Arc::new(Inner {
            start,
            entry,
            reserve,
            virtual_ns: AtomicU64::new(u64::MAX),
            retirement_ns: AtomicU64::new(u64::MAX),
        }))
    }
    pub fn deadline(&self) -> Instant {
        self.0.start + self.0.entry
    }
    pub fn entry(&self) -> Duration {
        self.0.entry
    }
    pub fn elapsed(&self) -> Duration {
        match self.0.virtual_ns.load(Ordering::SeqCst) {
            u64::MAX => self.0.start.elapsed(),
            n => Duration::from_nanos(n),
        }
    }
    pub fn now(&self) -> Instant {
        self.0.start + self.elapsed()
    }
    pub fn measurement(&self, duration: Duration) -> Result<()> {
        let end = self
            .elapsed()
            .checked_add(Duration::from_secs(110) + duration);
        if end.is_none_or(|end| end >= self.0.entry - self.0.reserve) {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn utc(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
    pub fn begin_retirement(&self) -> Result<()> {
        // 1115s exact CLI call graph +25 replay +20 file/fence allowance.
        let reserve = Duration::from_secs(1160);
        if self
            .elapsed()
            .checked_add(reserve)
            .is_none_or(|d| d > self.0.entry)
        {
            return Err(failure(124));
        }
        self.0
            .retirement_ns
            .store(reserve.as_nanos() as u64, Ordering::SeqCst);
        Ok(())
    }
    pub fn settled_slot(&self, timeout: Duration, grace: Duration) {
        let amount = (timeout + grace + SETTLEMENT).as_nanos() as u64;
        let _ = self
            .0
            .retirement_ns
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                if n == u64::MAX {
                    None
                } else {
                    Some(n.saturating_sub(amount))
                }
            });
    }
    pub fn latest(&self, timeout: Duration, grace: Duration, cleanup: bool) -> Result<Instant> {
        let limit = if cleanup {
            self.0.entry
        } else {
            self.0.entry - self.0.reserve
        };
        let reserved = self.0.retirement_ns.load(Ordering::SeqCst);
        let cost = if cleanup && reserved != u64::MAX {
            Duration::from_nanos(reserved).max(timeout + grace + SETTLEMENT)
        } else {
            timeout + grace + SETTLEMENT
        };
        let latest = limit.checked_sub(cost).ok_or_else(|| failure(124))?;
        if self.elapsed() > latest {
            return Err(failure(124));
        }
        Ok(self.0.start + latest)
    }
    pub fn fence(&self, cleanup: bool) -> Result<()> {
        if self.elapsed()
            >= if cleanup {
                self.0.entry
            } else {
                self.0.entry - self.0.reserve
            }
        {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub fn set(&self, d: Duration) {
        self.0
            .virtual_ns
            .store(d.as_nanos() as u64, Ordering::SeqCst);
    }
}
impl container_engine::Clock for Time {
    fn elapsed(&self) -> Duration {
        self.elapsed()
    }
    async fn sleep(&mut self, d: Duration) {
        if self.0.virtual_ns.load(Ordering::SeqCst) != u64::MAX {
            self.0
                .virtual_ns
                .fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
        } else {
            tokio::time::sleep(d).await;
        }
    }
}
pub(super) fn command(endpoint: &str, args: &[&str], cleanup: bool) -> Command {
    let mut c = Command::capture(Program::Docker, &["--host", endpoint]);
    c.arguments.extend(args.iter().map(OsString::from));
    c.capture = Some(CAPTURE);
    c.timeout = Duration::from_secs(if cleanup { 6 } else { 8 });
    c.grace = Duration::from_secs(1);
    c.cleanup = cleanup;
    c
}
pub(super) fn name(run: &str, role: Role) -> String {
    format!("scbench-{run}-{}", role.name())
}
pub(in crate::ci) const DAEMON: &str = concat!(
    "{\"id\":{{json .ID}},\"os\":{{json .OSType}},",
    "\"cgroup\":{{json .CgroupVersion}},\"system\":{{json .OperatingSystem}},",
    "\"security\":{{json .SecurityOptions}}}"
);
pub(super) const INSPECT: &str = concat!(
    "{\"id\":{{json .Id}},\"name\":{{json .Name}},\"image\":{{json .Image}},",
    "\"configuredImage\":{{json .Config.Image}},\"run\":{{json (index .Config.Labels ",
    "\"simplestchat.benchmark.run\")}},\"role\":{{json (index .Config.Labels ",
    "\"simplestchat.benchmark.role\")}},\"network\":{{json .HostConfig.NetworkMode}},",
    "\"state\":{\"Running\":{{json .State.Running}},\"Restarting\":{{json .State.Restarting}},",
    "\"OOMKilled\":{{json .State.OOMKilled}},\"ExitCode\":{{json .State.ExitCode}},",
    "\"ErrorEmpty\":{{eq .State.Error \"\"}},\"Pid\":{{json .State.Pid}},",
    "\"StartedAt\":{{json .State.StartedAt}}}}"
);
pub(super) fn allowed(program: Program, args: &[OsString]) -> bool {
    if program != Program::Docker {
        return false;
    }
    let Some(a) = args.iter().map(|v| v.to_str()).collect::<Option<Vec<_>>>() else {
        return false;
    };
    if a == [
        "context",
        "inspect",
        "--format",
        "{{.Endpoints.docker.Host}}",
    ] {
        return true;
    }
    a.len() >= 3
        && a[0] == "--host"
        && crate::ci::container_engine::endpoint(a[1], container_engine::Scope::Benchmark).is_ok()
        && recipe(&a[2..])
}

const PROTECTIONS: &[&str] = &[
    "--pull",
    "never",
    "--read-only",
    "--user",
    "10001:10001",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,noexec,size=64m",
    "--memory",
    "4g",
    "--memory-swap",
    "4g",
    "--cpus",
    "4",
    "--pids-limit",
    "1024",
    "--ulimit",
    "nofile=65536:65536",
    "--ulimit",
    "core=0:0",
    "--log-driver",
    "local",
    "--log-opt",
    "max-size=10m",
    "--log-opt",
    "max-file=3",
];
pub(super) fn create(
    endpoint: &str,
    s: &super::config::Settings,
    run: &str,
    role: Role,
    server: &str,
) -> Command {
    let name = name(run, role);
    let mut a = vec![
        "create".into(),
        "--name".into(),
        name,
        "--label".into(),
        format!("{RUN_KEY}={run}"),
        "--label".into(),
        format!("{ROLE_KEY}={}", role.name()),
    ];
    a.extend(PROTECTIONS.iter().map(|v| (*v).to_owned()));
    a.push("--network".into());
    a.push(if role == Role::Server {
        "none".into()
    } else {
        format!("container:{server}")
    });
    if role == Role::Server {
        for v in [
            "METRICS_TOKEN".to_owned(),
            "REGISTRATION_ENABLED=false".into(),
            "ALLOW_AD_HOC_ROOMS=true".into(),
            "RUN_MIGRATIONS=false".into(),
            "BIND_ADDR=127.0.0.1".into(),
            "PORT=3000".into(),
            "ANNOUNCE_IP=127.0.0.1".into(),
            "ALLOWED_ORIGINS=http://127.0.0.1:3000".into(),
            format!("MEDIA_WORKERS={}", s.workers),
            "WEBRTC_SERVER_PORT_BASE=40000".into(),
            "RUST_LOG=error".into(),
        ] {
            a.extend(["--env".into(), v]);
        }
        a.push(s.server_image.clone());
    } else {
        a.extend([
            "--env".into(),
            "RUST_LOG=error".into(),
            "--mount".into(),
            format!("type=bind,src={}/workload,dst=/results", s.output.display()),
            s.generator_image.clone(),
        ]);
        for (k, v) in [
            ("--server", "ws://127.0.0.1:3000/ws".into()),
            ("--clients", s.clients.to_string()),
            ("--rooms", s.rooms.to_string()),
            ("--room", format!("benchmark-{run}")),
            ("--duration", s.duration.to_string()),
            ("--ramp-up", s.ramp_up.to_string()),
            ("--warmup", s.warmup.to_string()),
            ("--quality", "480p".into()),
            ("--fps", "30".into()),
            ("--max-audio", "4".into()),
            ("--max-video", "4".into()),
            ("--output-dir", "/results".into()),
            ("--run-label", run.into()),
            ("--server-revision", s.server_image.clone()),
            ("--generator-revision", s.generator_image.clone()),
        ] {
            a.extend([k.into(), v]);
        }
    }
    command(
        endpoint,
        &a.iter().map(String::as_str).collect::<Vec<_>>(),
        false,
    )
}
pub(super) fn nonce(v: &str) -> bool {
    v.len() == 32
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn cid(v: &str) -> bool {
    crate::ci::container_engine::container_id(v)
}
fn image(v: &str) -> bool {
    crate::ci::container_engine::image_id(v)
}
fn fixed_create(a: &[&str]) -> bool {
    // Decode only closed recipe coordinates, then reconstruct the entire argv.
    if a.len() < 8 || a[..2] != ["create", "--name"] || a[3] != "--label" || a[5] != "--label" {
        return false;
    }
    let Some(run) = a[4].strip_prefix(&format!("{RUN_KEY}=")) else {
        return false;
    };
    if !nonce(run) {
        return false;
    }
    let role = if a[6] == format!("{ROLE_KEY}=server") {
        Role::Server
    } else if a[6] == format!("{ROLE_KEY}=generator") {
        Role::Generator
    } else {
        return false;
    };
    if a[2] != name(run, role) {
        return false;
    }
    let mut settings = super::config::Settings {
        server_image: format!("sha256:{}", "0".repeat(64)),
        generator_image: format!("sha256:{}", "0".repeat(64)),
        output: "/invalid".into(),
        workers: 1,
        clients: 10,
        rooms: 1,
        duration: 60,
        ramp_up: 5,
        warmup: 10,
    };
    let mut server = String::new();
    if role == Role::Server {
        let Some(last) = a.last() else {
            return false;
        };
        if !image(last) {
            return false;
        }
        settings.server_image = (*last).into();
        let Some(w) = a.iter().find_map(|v| v.strip_prefix("MEDIA_WORKERS=")) else {
            return false;
        };
        let Ok(w) = w.parse::<u32>() else {
            return false;
        };
        if !(1..=4).contains(&w) {
            return false;
        }
        settings.workers = w;
    } else {
        let Some(net) = a.windows(2).find(|p| p[0] == "--network") else {
            return false;
        };
        let Some(id) = net[1].strip_prefix("container:") else {
            return false;
        };
        if !cid(id) {
            return false;
        }
        server = id.into();
        let Some(mount) = a.windows(2).find(|p| p[0] == "--mount") else {
            return false;
        };
        let Some(out) = mount[1]
            .strip_prefix("type=bind,src=")
            .and_then(|v| v.strip_suffix("/workload,dst=/results"))
        else {
            return false;
        };
        if !super::config::path(std::path::Path::new(out)) {
            return false;
        }
        settings.output = out.into();
        let Some(pos) = a.iter().position(|v| *v == "--server") else {
            return false;
        };
        if pos == 0 || !image(a[pos - 1]) {
            return false;
        }
        settings.generator_image = a[pos - 1].into();
        let value = |key: &str| a[pos..].windows(2).find(|p| p[0] == key).map(|p| p[1]);
        let mut pairs = vec![
            "run".into(),
            "--output".into(),
            out.into(),
            "--server-image".into(),
            value("--server-revision").unwrap_or("").into(),
            "--generator-image".into(),
            settings.generator_image.clone(),
        ];
        for key in [
            "--clients",
            "--rooms",
            "--duration",
            "--ramp-up",
            "--warmup",
        ] {
            pairs.extend([key.into(), value(key).unwrap_or("").into()]);
        }
        let Ok(super::config::Config::Run(s)) = super::config::Config::parse(&pairs) else {
            return false;
        };
        settings = s;
    }
    let rebuilt = create("unix:///invalid", &settings, run, role, &server);
    rebuilt.arguments[2..]
        .iter()
        .map(|v| v.to_str().unwrap())
        .eq(a.iter().copied())
}
fn recipe(a: &[&str]) -> bool {
    match a {
        ["info", "--format", f] => *f == "{{.ID}}" || *f == DAEMON,
        [
            "ps",
            "--quiet",
            "--filter",
            "label=com.docker.compose.project=simplestchat-public",
        ] => true,
        ["image", "inspect", "--format", format, id] => {
            image(id)
                && matches!(
                    *format,
                    "{{.Id}}"
                        | "{{.Config.User}}"
                        | "{{json .Config.Env}}"
                        | "{{json .Config}}"
                        | "{{json .Config.Entrypoint}}"
                )
        }
        ["inspect", "--format", format, id] => *format == INSPECT && cid(id),
        ["start" | "wait" | "rm", id] => cid(id),
        ["stop", "--time", t, id] => matches!(*t, "5" | "30") && cid(id),
        ["kill", "--signal", "KILL", id] | ["logs", "--timestamps", id] => cid(id),
        ["stats", "--no-stream", "--format", "{{json .}}", s, g] => cid(s) && cid(g) && s != g,
        [
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--filter",
            filter,
            "--format",
            "{{.ID}}",
        ] => {
            filter.strip_prefix("id=").is_some_and(cid)
                || filter
                    .strip_prefix("name=^/scbench-")
                    .and_then(|v| v.strip_suffix('$'))
                    .is_some_and(|v| {
                        v.strip_suffix("-server")
                            .or_else(|| v.strip_suffix("-generator"))
                            .is_some_and(nonce)
                    })
        }
        _ => fixed_create(a),
    }
}
/// A proof additionally freezes this exact profile before Native transfers IO.
pub(super) fn profile(c: &Command) -> bool {
    if c.grace != Duration::from_secs(1) || c.stdout.is_some() {
        return false;
    }
    if c.program != Program::Docker {
        return false;
    }
    let Some(a) = c
        .arguments
        .iter()
        .map(|v| v.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    if a == [
        "context",
        "inspect",
        "--format",
        "{{.Endpoints.docker.Host}}",
    ] {
        return !c.cleanup
            && c.timeout <= Duration::from_secs(5)
            && c.capture.is_none()
            && c.separate_logs
                .as_ref()
                .is_some_and(|l| l.stdout.maximum() == 4096 && l.stderr.maximum() == CAPTURE)
            && c.combined_log.is_none()
            && c.stdin.is_none();
    }
    if a.len() < 3
        || crate::ci::container_engine::endpoint(a[1], container_engine::Scope::Benchmark).is_err()
        || a[0] != "--host"
        || !recipe(&a[2..])
    {
        return false;
    }
    let r = &a[2..];
    if c.stdin.is_some() {
        return false;
    }
    if c.cleanup && matches!(r[0], "create" | "start" | "wait" | "stats" | "image" | "ps") {
        return false;
    }
    let max = match r {
        ["info", ..] => 5,
        ["stop", "--time", "30", _] => 33,
        ["stop", "--time", "5", _] => 8,
        _ => {
            if c.cleanup {
                6
            } else {
                8
            }
        }
    };
    if c.timeout.is_zero() || c.timeout > Duration::from_secs(max) {
        return false;
    }
    if r[0] == "logs" {
        c.capture.is_none()
            && c.separate_logs.is_none()
            && c.combined_log.as_ref().is_some_and(|l| l.maximum() == LOG)
    } else {
        c.capture.is_none()
            && c.combined_log.is_none()
            && c.separate_logs
                .as_ref()
                .is_some_and(|l| l.stdout.maximum() == CAPTURE && l.stderr.maximum() == CAPTURE)
    }
}
