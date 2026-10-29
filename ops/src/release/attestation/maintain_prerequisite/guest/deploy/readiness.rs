//! The migration server must become ready before its deliberate, healthy stop.

use super::*;
use std::{
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
};

const READY_SECONDS: u64 = 45;

trait Migration {
    async fn ready(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    async fn healthy_exit(&mut self) -> Result<()>;
    async fn remove(&mut self) -> Result<()>;
}
async fn lifecycle(migration: &mut impl Migration) -> Result<()> {
    migration.ready().await?;
    migration.stop().await?;
    migration.healthy_exit().await?;
    migration.remove().await
}
fn identity(row: &Value, id: &str, image: &str, token: &str) -> Result<()> {
    require(
        policy::digest(id)
            && row["Id"] == id
            && row["Image"] == image
            && row["Config"]["Labels"][LABEL] == token
            && row["Config"]["Labels"]["com.docker.compose.project"] == PROJECT
            && row["Config"]["Labels"]["com.docker.compose.service"] == "migrate",
    )
}
#[derive(PartialEq, Eq)]
struct Running {
    pid: u32,
    started: String,
    restarts: u64,
}
fn running(row: &Value, id: &str, image: &str, token: &str) -> Result<Running> {
    identity(row, id, image, token)?;
    require(row["State"]["Running"] == true)?;
    let pid = row["State"]["Pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
        .ok_or(Error::Refused)?;
    let started = policy::string(&row["State"]["StartedAt"])?;
    require(!started.is_empty())?;
    Ok(Running {
        pid,
        started: started.into(),
        restarts: row["RestartCount"].as_u64().ok_or(Error::Refused)?,
    })
}
fn same_running(row: &Value, id: &str, image: &str, token: &str, before: &Running) -> Result<()> {
    require(running(row, id, image, token)? == *before)
}
fn stopped(row: &Value, id: &str, image: &str, token: &str, before: &Running) -> Result<()> {
    identity(row, id, image, token)?;
    require(
        row["State"]["StartedAt"] == before.started
            && row["RestartCount"].as_u64() == Some(before.restarts)
            && row["State"]["Running"] == false
            && row["State"]["OOMKilled"] == false
            && row["State"]["Error"] == ""
            && row["State"]["ExitCode"].as_i64() == Some(0),
    )
}
fn ready_response(status: i32, bytes: &[u8]) -> Result<bool> {
    if status != 0 {
        return Ok(false);
    }
    let value = policy::decode(bytes)?;
    policy::object(&value)?;
    Ok(value["status"] == "ready")
}
fn closed_probe(probed: Result<bool>, closed: Result<()>) -> Result<bool> {
    probed.and_then(|ready| closed.map(|()| ready))
}
fn namespace_stamp(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
struct Namespace {
    file: Option<File>,
    path: PathBuf,
    stamp: (u64, u64),
}
impl Namespace {
    fn open(pid: u32, control: &mut super::super::super::control::Control) -> Result<Self> {
        control.check()?;
        require(pid > 1)?;
        let path = PathBuf::from(format!("/proc/{pid}/ns/net"));
        // This exact procfs magic link selects the inspected namespace. The SDK
        // opens a read-only CLOEXEC descriptor, retained through child settlement.
        let file = File::open(&path).map_err(|_| Error::Io)?;
        let admitted = (|| {
            require(
                rustix::io::fcntl_getfd(&file)
                    .map_err(|_| Error::Io)?
                    .contains(rustix::io::FdFlags::CLOEXEC),
            )?;
            let stamp = namespace_stamp(&file.metadata().map_err(|_| Error::Io)?);
            require(stamp == namespace_stamp(&fs::metadata(&path).map_err(|_| Error::Changed)?))?;
            control.check()?;
            Ok(stamp)
        })();
        match admitted {
            Ok(stamp) => Ok(Self {
                file: Some(file),
                path,
                stamp,
            }),
            Err(error) => {
                control.record(error);
                let _ = super::super::super::files::close(file, control);
                Err(error)
            }
        }
    }
    fn argument(&self) -> Result<String> {
        let descriptor = self.file.as_ref().ok_or(Error::Close)?.as_raw_fd();
        Ok(format!(
            "--net=/proc/{}/fd/{descriptor}",
            std::process::id()
        ))
    }
    fn check(&self) -> Result<()> {
        require(
            namespace_stamp(
                &self
                    .file
                    .as_ref()
                    .ok_or(Error::Close)?
                    .metadata()
                    .map_err(|_| Error::Io)?,
            ) == self.stamp
                && namespace_stamp(&fs::metadata(&self.path).map_err(|_| Error::Changed)?)
                    == self.stamp,
        )
    }
    fn finish(&mut self, control: &mut super::super::super::control::Control) -> Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        super::super::super::files::close(file, control)
    }
}
async fn wait_ready(
    fixture: &mut Fixture,
    ctx: &mut Context,
    id: &str,
    image: &str,
) -> Result<Running> {
    let previous = ctx.control.push_phase(Duration::from_secs(READY_SECONDS))?;
    let result = async {
        // The frozen success=False probe inspects every actual Unix exit status.
        // A nonzero result retries; signals and the original owner fence still apply.
        let statuses = (-128..=255).collect::<Vec<i32>>();
        loop {
            fixture.check_monitors(ctx)?;
            let row = fixture.deploy_inspect(ctx, id).await?;
            let before = running(&row, id, image, &ctx.request.run_id)?;
            let mut namespace = Namespace::open(before.pid, &mut ctx.control)?;
            let probed = async {
                let row = fixture.deploy_inspect(ctx, id).await?;
                same_running(&row, id, image, &ctx.request.run_id, &before)?;
                namespace.check()?;
                let curl = ctx.commands.original("curl")?;
                curl.check_metadata(&mut ctx.control)?;
                let curl_path = format!(
                    "/proc/{}/fd/{}",
                    std::process::id(),
                    curl.file()?.as_raw_fd()
                );
                let args = vec![
                    "nsenter".into(),
                    namespace.argument()?,
                    "--".into(),
                    curl_path,
                    "--disable".into(),
                    "--noproxy".into(),
                    "*".into(),
                    "--proto".into(),
                    "=http".into(),
                    "--fail".into(),
                    "--silent".into(),
                    "--show-error".into(),
                    "--max-time".into(),
                    "2".into(),
                    "http://127.0.0.1:3000/ready".into(),
                ];
                let output = ctx
                    .commands
                    .execute(
                        &mut ctx.control,
                        "deploy-migrate-ready",
                        &args,
                        Duration::from_secs(5),
                        &statuses,
                    )
                    .await?;
                let row = fixture.deploy_inspect(ctx, id).await?;
                same_running(&row, id, image, &ctx.request.run_id, &before)?;
                namespace.check()?;
                ready_response(output.status, &output.stdout)
            }
            .await;
            if let Err(error) = probed {
                ctx.control.record(error);
            }
            let closed = namespace.finish(&mut ctx.control);
            if closed_probe(probed, closed)? {
                return Ok(before);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            ctx.control.check()?;
        }
    }
    .await;
    let ended = ctx.control.pop_phase(previous);
    result.and_then(|witness| ended.map(|()| witness))
}
struct Owned<'a> {
    fixture: &'a mut Fixture,
    ctx: &'a mut Context,
    id: &'a str,
    image: &'a str,
    ready: Option<Running>,
}
impl Migration for Owned<'_> {
    async fn ready(&mut self) -> Result<()> {
        self.ready = Some(wait_ready(self.fixture, self.ctx, self.id, self.image).await?);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Fixture::compose(
            self.ctx,
            "deploy-migrate-stop",
            &[
                "--profile".into(),
                "maintenance".into(),
                "stop".into(),
                "--timeout".into(),
                "30".into(),
                "migrate".into(),
            ],
            45,
        )
        .await
        .map(|_| ())
    }
    async fn healthy_exit(&mut self) -> Result<()> {
        let row = self.fixture.deploy_inspect(self.ctx, self.id).await?;
        stopped(
            &row,
            self.id,
            self.image,
            &self.ctx.request.run_id,
            self.ready.as_ref().ok_or(Error::Refused)?,
        )
    }
    async fn remove(&mut self) -> Result<()> {
        Fixture::docker(
            self.ctx,
            "deploy-migrate-remove",
            &["rm".into(), self.id.into()],
            15,
        )
        .await?;
        self.fixture.actors.remove(self.id);
        Ok(())
    }
}
pub(super) async fn complete(
    fixture: &mut Fixture,
    ctx: &mut Context,
    id: &str,
    image: &str,
) -> Result<()> {
    lifecycle(&mut Owned {
        fixture,
        ctx,
        id,
        image,
        ready: None,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    struct LongRunning {
        running: bool,
        readiness: Result<()>,
        ready: bool,
        removed: bool,
        calls: Vec<&'static str>,
        stop_failure: bool,
    }
    impl Migration for LongRunning {
        async fn ready(&mut self) -> Result<()> {
            self.calls.push("ready");
            require(self.running)?;
            self.readiness?;
            self.ready = true;
            Ok(())
        }
        async fn stop(&mut self) -> Result<()> {
            self.calls.push("stop");
            require(self.running && self.ready)?;
            if self.stop_failure {
                return Err(Error::Command);
            }
            self.running = false;
            Ok(())
        }
        async fn healthy_exit(&mut self) -> Result<()> {
            self.calls.push("exit");
            require(!self.running && self.ready)
        }
        async fn remove(&mut self) -> Result<()> {
            self.calls.push("remove");
            require(!self.running && self.ready)?;
            self.removed = true;
            Ok(())
        }
    }
    fn migration() -> LongRunning {
        LongRunning {
            running: true,
            readiness: Ok(()),
            ready: false,
            removed: false,
            calls: Vec::new(),
            stop_failure: false,
        }
    }
    #[tokio::test]
    async fn long_running_migration_is_ready_before_stop_and_only_then_exits_and_is_removed() {
        let mut server = migration();
        assert_eq!(server.healthy_exit().await, Err(Error::Refused));
        assert_eq!(server.stop().await, Err(Error::Refused));
        server.calls.clear();
        lifecycle(&mut server).await.unwrap();
        assert_eq!(server.calls, ["ready", "stop", "exit", "remove"]);
        assert!(!server.running && server.removed);
        for error in [Error::Deadline, Error::Refused, Error::Io] {
            let mut server = migration();
            server.readiness = Err(error);
            assert_eq!(lifecycle(&mut server).await, Err(error));
            assert_eq!(server.calls, ["ready"]);
            assert!(server.running && !server.removed);
        }
        let mut server = migration();
        server.stop_failure = true;
        assert_eq!(lifecycle(&mut server).await, Err(Error::Command));
        assert_eq!(server.calls, ["ready", "stop"]);
        assert!(server.running && !server.removed);
    }
    #[test]
    fn only_the_owned_running_migration_pid_and_valid_ready_record_can_advance() {
        let id = "a".repeat(64);
        let image = format!("sha256:{}", "b".repeat(64));
        let token = "c".repeat(32);
        let row = json!({"Id":id,"Image":image,"Config":{"Labels":{(LABEL):token,
            "com.docker.compose.project":PROJECT,"com.docker.compose.service":"migrate"}},
            "RestartCount":0,"State":{"Running":true,"Pid":123,
                "StartedAt":"2026-10-29T00:00:00Z"}});
        let before = running(&row, &id, &image, &token).unwrap();
        assert_eq!(before.pid, 123);
        for pointer in [
            "/Id",
            "/Image",
            "/Config/Labels/simplestchat.maintain-prerequisite",
            "/Config/Labels/com.docker.compose.project",
            "/Config/Labels/com.docker.compose.service",
        ] {
            let mut changed = row.clone();
            *changed.pointer_mut(pointer).unwrap() = "wrong".into();
            assert!(same_running(&changed, &id, &image, &token, &before).is_err());
        }
        for pid in [json!(0), json!(1), json!(-1), json!(u64::MAX), Value::Null] {
            let mut changed = row.clone();
            changed["State"]["Pid"] = pid;
            assert!(same_running(&changed, &id, &image, &token, &before).is_err());
        }
        for (pointer, replacement) in [
            ("/State/Pid", json!(124)),
            ("/State/StartedAt", json!("2026-10-29T00:00:01Z")),
            ("/RestartCount", json!(1)),
        ] {
            let mut changed = row.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            assert!(same_running(&changed, &id, &image, &token, &before).is_err());
        }
        let mut changed = row.clone();
        changed["State"]["Running"] = false.into();
        assert!(same_running(&changed, &id, &image, &token, &before).is_err());
        let mut finished = row;
        finished["State"]["Running"] = false.into();
        finished["State"]["Pid"] = 0.into();
        finished["State"]["OOMKilled"] = false.into();
        finished["State"]["Error"] = "".into();
        finished["State"]["ExitCode"] = 0.into();
        stopped(&finished, &id, &image, &token, &before).unwrap();
        for (pointer, replacement) in [
            ("/State/StartedAt", json!("later")),
            ("/RestartCount", json!(1)),
            ("/State/ExitCode", json!(1)),
            ("/State/OOMKilled", json!(true)),
            ("/State/Error", json!("failure")),
        ] {
            let mut changed = finished.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            assert!(stopped(&changed, &id, &image, &token, &before).is_err());
        }
        assert_eq!(ready_response(0, br#"{"status":"ready"}"#), Ok(true));
        assert_eq!(ready_response(7, b""), Ok(false));
        assert_eq!(ready_response(0, br#"{"status":"starting"}"#), Ok(false));
        for bytes in [
            b"null".as_slice(),
            b"[]",
            b"{\"status\":\"ready\",\"status\":\"ready\"}",
            b"{\"status\":\"ready\"}\n{}",
        ][..]
            .iter()
        {
            assert!(ready_response(0, bytes).is_err());
        }
        assert_eq!(closed_probe(ready_response(7, b""), Ok(())), Ok(false));
        assert_eq!(
            closed_probe(ready_response(0, br#"{"status":"starting"}"#), Ok(())),
            Ok(false)
        );
        assert_eq!(
            closed_probe(ready_response(0, br#"{"status":"ready"}"#), Ok(())),
            Ok(true)
        );
        assert_eq!(
            closed_probe(Ok(false), Err(Error::Close)),
            Err(Error::Close)
        );
        assert_eq!(
            closed_probe(Err(Error::Refused), Err(Error::Close)),
            Err(Error::Refused)
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn namespace_is_held_cloexec_by_the_parent_until_explicit_checked_close() {
        let mut control =
            super::super::super::super::control::Control::new_deploy(Instant::now()).unwrap();
        let mut namespace = Namespace::open(std::process::id(), &mut control).unwrap();
        namespace.check().unwrap();
        let argument = namespace.argument().unwrap();
        let path = argument.strip_prefix("--net=").unwrap();
        assert_eq!(
            namespace_stamp(&fs::metadata(path).unwrap()),
            namespace.stamp
        );
        assert!(
            rustix::io::fcntl_getfd(namespace.file.as_ref().unwrap())
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        namespace.finish(&mut control).unwrap();
        assert!(namespace.file.is_none());
        assert_eq!(namespace.argument(), Err(Error::Close));
        control.finish().unwrap();
    }
}
