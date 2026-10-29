//! One owned negative-only inspection service. No response precedes the real stop.
use super::*;
use std::{
    fs::{self, File, Metadata},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
};

pub(in super::super) use super::injection_client::{client, enter};
const CONFIG_PATH: &str = "/root/simplestchat-vm-fixture/deploy-injection-config.json";
const SOCKET: &str = "/root/simplestchat-vm-fixture/deploy-injection.sock";
const RECEIPT: &str = "/root/simplestchat-vm-fixture/deploy-injection-receipt.json";
const FORMAT: &str = concat!(
    "{\"id\":{{json .Id}},\"image\":{{json .Image}},\"state\":{{json .State}},",
    "\"restarts\":{{.RestartCount}},",
    "\"configHash\":{{json (index .Config.Labels \"com.docker.compose.config-hash\")}}}",
);
fn binding(meta: &Metadata) -> (u64, u64, u32, u32, u32, u64) {
    (
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.uid(),
        meta.gid(),
        meta.nlink(),
    )
}
fn full_stamp(meta: &Metadata) -> Result<Value> {
    let nanos = |seconds: i64, fraction: i64| {
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(fraction))
            .ok_or(Error::Bounds)
    };
    Ok(json!([
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.uid(),
        meta.gid(),
        meta.nlink(),
        meta.len(),
        nanos(meta.mtime(), meta.mtime_nsec())?,
        nanos(meta.ctime(), meta.ctime_nsec())?
    ]))
}
pub(super) fn finished_at(value: &Value) -> Result<&str> {
    let text = value.as_str().ok_or(Error::Refused)?;
    require(!text.is_empty() && text.len() <= 64 && text.is_ascii())?;
    let bytes = text.as_bytes();
    require(bytes.len() >= 20)?;
    for (at, byte) in bytes[..19].iter().enumerate() {
        let punctuation = match at {
            4 | 7 => Some(b'-'),
            10 => Some(b'T'),
            13 | 16 => Some(b':'),
            _ => None,
        };
        require(punctuation.map_or_else(|| byte.is_ascii_digit(), |expected| *byte == expected))?;
    }
    let mut suffix = &bytes[19..];
    if suffix.first() == Some(&b'.') {
        suffix = &suffix[1..];
        let count = suffix
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        require((1..=9).contains(&count))?;
        suffix = &suffix[count..];
    }
    require(
        suffix == b"Z"
            || (suffix.len() == 6
                && b"+-".contains(&suffix[0])
                && suffix[3] == b':'
                && [1, 2, 4, 5].iter().all(|at| suffix[*at].is_ascii_digit())),
    )?;
    Ok(text)
}
fn absolute_end(control: &mut Control) -> Result<f64> {
    control.check()?;
    // Kernel sampling precedes the later Instant sample. Two downward roundings
    // also cover floating conversion/addition, without extending the original end.
    let clock = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    require(clock.tv_sec >= 0 && (0..1_000_000_000).contains(&clock.tv_nsec))?;
    let sampled = clock.tv_sec as f64 + clock.tv_nsec as f64 / 1_000_000_000.0;
    let remaining = control
        .body_end()
        .checked_duration_since(Instant::now())
        .ok_or(Error::Deadline)?;
    let end = (sampled + remaining.as_secs_f64()).next_down().next_down();
    require(end.is_finite() && end > sampled)?;
    Ok(end)
}
fn active(ctx: &mut Context, end: Instant) -> Result<()> {
    ctx.control.check()?;
    ctx.wire.no_extra(&mut ctx.control)?;
    if Instant::now() >= end {
        return Err(Error::Deadline);
    }
    Ok(())
}
fn remaining(ctx: &mut Context, end: Instant) -> Result<Duration> {
    active(ctx, end)?;
    end.checked_duration_since(Instant::now())
        .ok_or(Error::Deadline)
}
fn read(peer: &mut UnixStream, count: usize, ctx: &mut Context, end: Instant) -> Result<Vec<u8>> {
    require(count <= 2 * 1024 * 1024)?;
    let mut bytes = vec![0; count];
    let mut at = 0;
    while at < count {
        peer.set_read_timeout(Some(remaining(ctx, end)?))
            .map_err(|_| Error::Io)?;
        let limit = (at + 65536).min(count);
        match peer.read(&mut bytes[at..limit]) {
            Ok(0) => return Err(Error::Refused),
            Ok(size) => at += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Io),
        }
    }
    active(ctx, end)?;
    Ok(bytes)
}
fn write(peer: &mut UnixStream, bytes: &[u8], ctx: &mut Context, end: Instant) -> Result<()> {
    require(bytes.len() <= 2 * 1024 * 1024)?;
    let mut at = 0;
    while at < bytes.len() {
        peer.set_write_timeout(Some(remaining(ctx, end)?))
            .map_err(|_| Error::Io)?;
        match peer.write(&bytes[at..(at + 65536).min(bytes.len())]) {
            Ok(0) => return Err(Error::Io),
            Ok(size) => at += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Io),
        }
    }
    active(ctx, end)
}
fn receive(peer: &mut UnixStream, ctx: &mut Context, end: Instant) -> Result<Value> {
    let prefix = read(peer, 4, ctx, end)?;
    let count = u32::from_be_bytes(prefix.try_into().map_err(|_| Error::Input)?) as usize;
    require((1..=16384).contains(&count))?;
    decode(&read(peer, count, ctx, end)?)
}
fn send(peer: &mut UnixStream, value: &Value, ctx: &mut Context, end: Instant) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Input)?;
    require((1..=16384).contains(&bytes.len()))?;
    write(peer, &(bytes.len() as u32).to_be_bytes(), ctx, end)?;
    write(peer, &bytes, ctx, end)
}
fn quiet(peer: &UnixStream, ctx: &mut Context, end: Instant) -> Result<()> {
    active(ctx, end)?;
    let mut byte = [0u8; 1];
    match rustix::net::recv(
        peer,
        &mut byte,
        rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT,
    ) {
        Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => active(ctx, end),
        _ => Err(Error::Refused),
    }
}
#[cfg(target_os = "linux")]
fn process_parent(pid: u32, ctx: &mut Context) -> Result<u32> {
    ctx.control.check()?;
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    let file = File::from(
        rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::Io)?,
    );
    let body = (|| {
        let mut bytes = Vec::new();
        (&file)
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Io)?;
        require(bytes.len() <= 16384 && bytes.starts_with(format!("{pid} (").as_bytes()))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::Refused)?;
        let (_, tail) = text.rsplit_once(") ").ok_or(Error::Refused)?;
        let parent = tail.split_whitespace().nth(1).ok_or(Error::Refused)?;
        require(!parent.is_empty() && parent.bytes().all(|byte| byte.is_ascii_digit()))?;
        parent
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or(Error::Refused)
    })();
    if let Err(error) = body {
        ctx.control.record(error);
    }
    let closed = super::super::files::close(file, &mut ctx.control);
    body.and_then(|value| closed.map(|()| value))
}
pub(super) struct Injector {
    root: Option<File>,
    root_stamp: Option<Metadata>,
    listener: Option<UnixListener>,
    socket_stamp: Option<Metadata>,
    leader: Option<u32>,
    requests: u16,
    baseline: BTreeSet<String>,
    dependencies: Value,
    selected: BTreeMap<String, Vec<u8>>,
    before: BTreeSet<String>,
    image: String,
    rollback: String,
    old: String,
    id: Option<String>,
    stopped: bool,
    closed: bool,
    close_error: Option<Error>,
}
impl Injector {
    pub fn new(
        ctx: &mut Context,
        baseline: &effects::deploy::Baseline,
        image: &str,
        rollback: &str,
        old: &str,
        before: &BTreeSet<String>,
        selected: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Self> {
        let mut owned = Self {
            root: None,
            root_stamp: None,
            listener: None,
            socket_stamp: None,
            leader: None,
            requests: 0,
            baseline: BTreeSet::from([baseline.pg.clone(), baseline.proxy.clone(), old.into()]),
            dependencies: baseline.dependencies.clone(),
            selected: selected.clone(),
            before: before.clone(),
            image: image.into(),
            rollback: rollback.into(),
            old: old.into(),
            id: None,
            stopped: false,
            closed: false,
            close_error: None,
        };
        let body = (|| {
            ctx.check()?;
            require(hex(old, 64) && image != rollback)?;
            require(fs::canonicalize(ROOT).map_err(|_| Error::Io)? == Path::new(ROOT))?;
            owned.root = Some(File::from(
                rustix::fs::open(
                    ROOT,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|_| Error::Io)?,
            ));
            let stamp = owned
                .root
                .as_ref()
                .ok_or(Error::Close)?
                .metadata()
                .map_err(|_| Error::Io)?;
            require(stamp.uid() == 0 && stamp.gid() == 0 && stamp.mode() & 0o7777 == 0o700)?;
            owned.root_stamp = Some(stamp);
            // The mature SDK creates its returned socket with CLOEXEC atomically.
            owned.listener = Some(UnixListener::bind(SOCKET).map_err(|_| Error::Io)?);
            owned.socket_stamp = Some(fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?);
            owned
                .listener
                .as_ref()
                .ok_or(Error::Close)?
                .set_nonblocking(true)
                .map_err(|_| Error::Io)?;
            rustix::net::listen(owned.listener.as_ref().ok_or(Error::Close)?, 1)
                .map_err(|_| Error::Io)?;
            fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o600))
                .map_err(|_| Error::Io)?;
            owned.socket_stamp = Some(fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?);
            ctx.originals
                .push(super::super::files::injection_alias(&mut ctx.control)?);
            let native = ctx.commands.original("ops")?;
            let docker = ctx.commands.original("docker")?;
            let config = json!({"schemaVersion":1,"nonce":ctx.request.run_id,
                "bodyEnd":absolute_end(&mut ctx.control)?,"image":image,
                "realCliSha256":docker.sha256,"realCliStamp":full_stamp(&docker.file()?
                    .metadata().map_err(|_| Error::Io)?)?,"nativeSha256":native.sha256,
                "nativeStamp":full_stamp(&native.file()?.metadata().map_err(|_| Error::Io)?)?,
                "namespaceInode":fs::metadata("/proc/self/ns/mnt").map_err(|_| Error::Io)?.ino()});
            owned.record(
                ctx,
                Path::new(CONFIG_PATH),
                &serde_json::to_vec(&config).map_err(|_| Error::Input)?,
            )?;
            owned.check(ctx)
        })();
        if let Err(error) = body {
            ctx.control.record(error);
            let _ = owned.finish(ctx);
            return Err(error);
        }
        Ok(owned)
    }
    fn check(&self, ctx: &mut Context) -> Result<()> {
        ctx.check()?;
        require(!self.closed)?;
        let root = self.root.as_ref().ok_or(Error::Close)?;
        let expected = self.root_stamp.as_ref().ok_or(Error::Close)?;
        require(
            binding(expected) == binding(&root.metadata().map_err(|_| Error::Io)?)
                && binding(expected)
                    == binding(&fs::symlink_metadata(ROOT).map_err(|_| Error::Io)?),
        )?;
        let expected = self.socket_stamp.as_ref().ok_or(Error::Close)?;
        let visible = fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?;
        require(
            visible.file_type().is_socket()
                && visible.uid() == 0
                && visible.gid() == 0
                && visible.mode() & 0o7777 == 0o600
                && binding(expected) == binding(&visible),
        )
    }
    fn record(&self, ctx: &mut Context, path: &Path, bytes: &[u8]) -> Result<String> {
        self.check(ctx)?;
        let original = super::super::files::write_new(path, bytes, 0, 0o600, &mut ctx.control)?;
        let digest = original.sha256.clone();
        ctx.originals.push(original);
        self.root
            .as_ref()
            .ok_or(Error::Close)?
            .sync_all()
            .map_err(|_| Error::Io)?;
        self.check(ctx)?;
        Ok(digest)
    }
    pub fn arguments(&self, ctx: &mut Context, revision: &str) -> Result<Vec<String>> {
        self.check(ctx)?;
        require(hex(revision, 40))?;
        let native = ctx.commands.original("ops")?.file()?.as_raw_fd();
        Ok(vec![
            "unshare".into(),
            "--mount".into(),
            "--propagation".into(),
            "private".into(),
            "--fork".into(),
            "--kill-child=TERM".into(),
            format!("/proc/{}/fd/{native}", std::process::id()),
            "release".into(),
            "maintain-prerequisite".into(),
            "deploy-injection".into(),
            "--enter".into(),
            "__SC_DEPLOY_FD__".into(),
            revision.into(),
        ])
    }
    pub fn leader(&mut self, pid: rustix::process::Pid) {
        self.leader = u32::try_from(pid.as_raw_nonzero().get()).ok();
    }
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn identifier(&self) -> Option<&str> {
        self.id.as_deref()
    }
    #[cfg(target_os = "linux")]
    fn credentials(&self, peer: &UnixStream, ctx: &mut Context) -> Result<()> {
        let credentials = rustix::net::sockopt::socket_peercred(peer).map_err(|_| Error::Io)?;
        require(credentials.uid.as_raw() == 0 && credentials.gid.as_raw() == 0)?;
        let pid =
            u32::try_from(credentials.pid.as_raw_nonzero().get()).map_err(|_| Error::Refused)?;
        let leader = self.leader.ok_or(Error::Refused)?;
        require(pid > 1)?;
        let parent = process_parent(pid, ctx)?;
        require(parent > 1)?;
        require(process_parent(parent, ctx)? == leader)
    }
    #[cfg(not(target_os = "linux"))]
    fn credentials(&self, _peer: &UnixStream, _ctx: &mut Context) -> Result<()> {
        Err(Error::Refused)
    }
    pub async fn poll(&mut self, ctx: &mut Context, fixture: &mut effects::Fixture) -> Result<()> {
        self.check(ctx)?;
        let (mut peer, _) = match self.listener.as_ref().ok_or(Error::Close)?.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(_) => return Err(Error::Io),
        };
        let end = ctx
            .control
            .body_end()
            .min(Instant::now() + Duration::from_secs(20));
        let mut phase = None;
        let body = async {
            self.requests = self.requests.checked_add(1).ok_or(Error::Bounds)?;
            require(self.requests <= 192)?;
            let cap = remaining(ctx, end)?;
            phase = Some(ctx.control.push_phase(cap)?);
            self.credentials(&peer, ctx)?;
            let value = receive(&mut peer, ctx, end)?;
            super::super::protocol::exact(&value, &["schemaVersion", "nonce", "identifier"])?;
            require(
                value["schemaVersion"].as_u64() == Some(1) && value["nonce"] == ctx.request.run_id,
            )?;
            let id = value["identifier"]
                .as_str()
                .filter(|id| hex(id, 64))
                .ok_or(Error::Refused)?;
            let receipt = self.deliver(ctx, fixture, &mut peer, id, end).await?;
            let acknowledged = receive(&mut peer, ctx, end)?;
            require(
                acknowledged
                    == json!({"action":"received","nonce":ctx.request.run_id,
                "receiptSha256":receipt}),
            )?;
            quiet(&peer, ctx, end)?;
            send(
                &mut peer,
                &json!({"action":"complete","nonce":ctx.request.run_id,
                "receiptSha256":receipt}),
                ctx,
                end,
            )?;
            quiet(&peer, ctx, end)
        }
        .await;
        if let Err(error) = body {
            ctx.control.record(error);
        }
        let closed = nix::unistd::close(peer).map_err(|_| Error::Close);
        if let Err(error) = closed {
            ctx.control.cleanup_failed = true;
            ctx.control.record(error);
        }
        let ended = phase.map_or(Ok(()), |prior| ctx.control.pop_phase(prior));
        body.and(closed).and(ended)
    }
    async fn deliver(
        &mut self,
        ctx: &mut Context,
        fixture: &mut effects::Fixture,
        peer: &mut UnixStream,
        id: &str,
        end: Instant,
    ) -> Result<Option<String>> {
        quiet(peer, ctx, end)?;
        if self.baseline.contains(id) || self.stopped {
            if !self.baseline.contains(id) {
                require(self.id.as_deref() != Some(id))?;
                self.rollback_peer(ctx, fixture, id).await?;
            }
            send(peer, &json!({"action":"pass"}), ctx, end)?;
            return Ok(None);
        }
        require(self.id.is_none() && id != self.old)?;
        let attempt = super::operation::attempt(&self.before)?;
        require(
            super::operation::read_record(
                ctx,
                &Path::new(PUBLIC).join("release-state.json"),
                0o600,
                131072,
            )? == json!({"schemaVersion":1,"attempt":attempt,
                "phase":"replace_application","finalized":false}),
        )?;
        require(effects::Fixture::deploy_running_app(ctx).await? == id)?;
        let before = fixture.deploy_inspect(ctx, id).await?;
        fixture.deploy_bind_app(ctx, &before, id, &self.image)?;
        let args = vec![
            "docker".into(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "inspect".into(),
            "--format".into(),
            FORMAT.into(),
            id.into(),
        ];
        let response = ctx
            .commands
            .execute(
                &mut ctx.control,
                "deploy-inject-real-inspect",
                &args,
                Duration::from_secs(15),
                &[0],
            )
            .await?;
        let value = decode(&response.stdout)?;
        super::super::protocol::exact(&value, &["id", "image", "state", "restarts", "configHash"])?;
        require(
            value["id"] == id
                && value["image"] == self.image
                && value["state"]["Running"] == true
                && value["state"]["OOMKilled"] == false,
        )?;
        let prior = finished_at(&value["state"]["FinishedAt"])?;
        quiet(peer, ctx, end)?;
        effects::Fixture::deploy_docker(
            ctx,
            "deploy-inject-stop",
            &["stop".into(), "--time".into(), "0".into(), id.into()],
            10,
        )
        .await?;
        let after = fixture.deploy_inspect(ctx, id).await?;
        let finished = finished_at(&after["State"]["FinishedAt"])?;
        require(
            after["Id"] == id
                && after["Image"] == self.image
                && after["State"]["Running"] == false
                && finished != prior
                && finished != "0001-01-01T00:00:00Z"
                && finished.len() <= 64,
        )?;
        self.id = Some(id.into());
        self.stopped = true;
        quiet(peer, ctx, end)?;
        let receipt = json!({"schemaVersion":1,"scope":"HostDeployInspectionBarrier",
            "nonce":ctx.request.run_id,"identifier":id,"image":self.image,"responseRawExit":0,
            "stdoutBytes":response.stdout.len(),"stdoutSha256":hash(&response.stdout),
            "stderrBytes":response.stderr.len(),"stderrSha256":hash(&response.stderr),
            "runningBefore":true,"runningAfter":false,"stopRawExit":0,"finishedChanged":true});
        let receipt = self.record(
            ctx,
            Path::new(RECEIPT),
            &serde_json::to_vec(&receipt).map_err(|_| Error::Input)?,
        )?;
        quiet(peer, ctx, end)?;
        send(
            peer,
            &json!({"action":"deliver","stdoutBytes":response.stdout.len(),
            "stderrBytes":response.stderr.len(),"receiptSha256":receipt}),
            ctx,
            end,
        )?;
        write(peer, &response.stdout, ctx, end)?;
        write(peer, &response.stderr, ctx, end)?;
        Ok(Some(receipt))
    }
    async fn rollback_peer(
        &self,
        ctx: &mut Context,
        fixture: &mut effects::Fixture,
        id: &str,
    ) -> Result<()> {
        require(self.stopped && self.id.as_deref() != Some(id) && self.image != self.rollback)?;
        require(super::operation::selection(ctx)? == self.selected)?;
        let attempt = super::operation::attempt(&self.before)?;
        require(
            super::operation::read_record(
                ctx,
                &Path::new(PUBLIC).join("release-state.json"),
                0o600,
                131072,
            )? == json!({"schemaVersion":1,"attempt":attempt,
                "phase":"replace_application","finalized":false}),
        )?;
        require(effects::Fixture::deploy_running_app(ctx).await? == id)?;
        let row = fixture.deploy_inspect(ctx, id).await?;
        fixture.deploy_bind_app(ctx, &row, id, &self.rollback)?;
        require(fixture.deploy_dependencies(ctx).await? == self.dependencies)?;
        require(super::operation::selection(ctx)? == self.selected)?;
        self.check(ctx)
    }
    pub fn finish(&mut self, ctx: &mut Context) -> Result<()> {
        if self.closed {
            return self.close_error.map_or(Ok(()), Err);
        }
        self.closed = true;
        let admitted = ctx.control.check();
        let mut first = admitted.err();
        if let Some(listener) = self.listener.take()
            && nix::unistd::close(listener).is_err()
        {
            first.get_or_insert(Error::Close);
        }
        if admitted.is_ok()
            && let Some(expected) = &self.socket_stamp
        {
            let unlinked = (|| {
                let visible = fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?;
                require(binding(expected) == binding(&visible))?;
                fs::remove_file(SOCKET).map_err(|_| Error::Io)?;
                self.root
                    .as_ref()
                    .ok_or(Error::Close)?
                    .sync_all()
                    .map_err(|_| Error::Io)
            })();
            if let Err(error) = unlinked {
                first.get_or_insert(error);
            }
        }
        if let Some(root) = self.root.take()
            && let Err(error) = super::super::files::close(root, &mut ctx.control)
        {
            first.get_or_insert(error);
        }
        if let Err(error) = ctx.control.check() {
            first.get_or_insert(error);
        }
        if let Some(error) = first {
            ctx.control.cleanup_failed = true;
            ctx.control.record(error);
            self.close_error = Some(error);
        }
        first.map_or(Ok(()), Err)
    }
}
