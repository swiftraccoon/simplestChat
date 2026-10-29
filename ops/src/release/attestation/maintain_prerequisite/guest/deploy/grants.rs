//! Feed the retained psql script through stdin, preserving its meta-commands.

use super::super::super::{control::Control, process::Commands};
use super::*;
use std::fs::File;

const MAXIMUM: u64 = 1024 * 1024;
const COMMAND_SECONDS: u64 = 30;

fn arguments(pg: &str) -> Result<Vec<String>> {
    require(policy::digest(pg))?;
    Ok([
        "docker",
        "--host",
        "unix:///var/run/docker.sock",
        "exec",
        "--interactive",
        "--user",
        "999:999",
        pg,
        "timeout",
        "--kill-after=2s",
        "15s",
        "psql",
        "--no-psqlrc",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        "/run/simplestchat-postgres",
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--tuples-only",
        "--no-align",
    ]
    .map(str::to_owned)
    .into())
}

trait Transport {
    async fn input(&mut self, control: &mut Control, args: &[String], file: &File) -> Result<()>;
}
impl Transport for Commands {
    async fn input(&mut self, control: &mut Control, args: &[String], file: &File) -> Result<()> {
        self.execute_input(
            control,
            "deploy-runtime-grants",
            args,
            file,
            Duration::from_secs(COMMAND_SECONDS),
        )
        .await
        .map(|_| ())
    }
}

async fn execute(
    source: &Original,
    transport: &mut impl Transport,
    control: &mut Control,
    pg: &str,
) -> Result<()> {
    // Preserve the original full read/hash gates. These bytes never enter argv.
    let bytes = source.data(MAXIMUM, control)?;
    std::str::from_utf8(&bytes).map_err(|_| Error::Refused)?;
    let args = arguments(pg)?;
    let body = transport.input(control, &args, source.file()?).await;
    if let Err(error) = body {
        control.record(error);
    }
    let replay = source.check(control);
    if let Err(error) = replay {
        control.record(error);
    }
    body.and(replay)
}

pub(super) async fn apply(fixture: &mut Fixture, ctx: &mut Context) -> Result<()> {
    ctx.check()?;
    let pg = fixture.pg.as_deref().ok_or(Error::Refused)?;
    let path = Path::new(CONFIG).join("runtime-grants.sql");
    let index = ctx
        .originals
        .iter()
        .position(|source| source.path == path)
        .ok_or(Error::Input)?;
    execute(
        &ctx.originals[index],
        &mut ctx.commands,
        &mut ctx.control,
        pg,
    )
    .await?;
    ctx.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned_process::{Group, exited, finish_before, reserved};
    use std::{
        io::{Read, Seek, SeekFrom},
        os::unix::{fs::PermissionsExt, process::CommandExt},
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const CHILD: &str = "SIMPLESTCHAT_GRANTS_STDIN_CHILD";
    const SCRIPT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/ansible/templates/public-runtime-grants.sql.j2"
    ));
    const SENTINEL: &[u8] = b"\nSELECT '__SC_GRANTS_STDIN_SENTINEL__';\n";

    struct Files(PathBuf);
    impl Files {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "deploy-grants-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let files = Self(fs::canonicalize(path).unwrap());
            fs::write(files.0.join("input"), [SCRIPT, SENTINEL].concat()).unwrap();
            fs::set_permissions(files.0.join("input"), fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(files.0.join("received"), b"").unwrap();
            files
        }
        fn remove(&self) -> std::io::Result<()> {
            fs::remove_file(self.0.join("input"))?;
            if self.0.join("received").exists() {
                fs::remove_file(self.0.join("received"))?;
            }
            fs::remove_dir(&self.0)
        }
    }
    impl Drop for Files {
        fn drop(&mut self) {
            let _ = self.remove();
        }
    }

    #[test]
    fn inert_stdin_child() {
        let Ok(mode) = std::env::var(CHILD) else {
            return;
        };
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAXIMUM + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, [SCRIPT, SENTINEL].concat());
        fs::write(std::env::var("SC_GRANTS_RECEIVED").unwrap(), &bytes).unwrap();
        assert_ne!(
            mode, "nonzero",
            "controlled nonzero child after the actual stdin read"
        );
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Case {
        Success,
        Nonzero,
        Changed,
        Close,
        FirstCause,
    }
    struct Inert<'a> {
        files: &'a Files,
        case: Case,
        calls: usize,
        seen: Vec<String>,
    }
    impl Transport for Inert<'_> {
        async fn input(
            &mut self,
            control: &mut Control,
            args: &[String],
            file: &File,
        ) -> Result<()> {
            self.calls += 1;
            self.seen = args.to_vec();
            // Exercise a real retained FD -> Stdio transfer, outside fixed ROOT/PRIVATE.
            // As in execute_input, stdlib owns the duplicate after transfer; its close
            // status is opaque. The original and child remain explicitly owned.
            let mut duplicate = file.try_clone().map_err(|_| Error::Io)?;
            if duplicate.seek(SeekFrom::Start(0)).is_err() {
                let error = Error::Io;
                control.record(error);
                let _ = super::super::super::super::files::close(duplicate, control);
                return Err(error);
            }
            let name = concat!(module_path!(), "::inert_stdin_child");
            let name = name.split_once("::").unwrap().1;
            let mut command = Command::new(std::env::current_exe().map_err(|_| Error::Io)?);
            command
                .args(["--exact", name, "--test-threads=1"])
                .env_clear()
                .env(
                    CHILD,
                    if self.case == Case::Nonzero {
                        "nonzero"
                    } else {
                        "success"
                    },
                )
                .env("SC_GRANTS_RECEIVED", self.files.0.join("received"))
                .current_dir(&self.files.0)
                .stdin(Stdio::from(duplicate))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let mut group = Group::new(command.spawn().map_err(|_| Error::Io)?);
            let pid = group.pid();
            let end = control.phase_end();
            let natural = tokio::time::timeout_at(
                tokio::time::Instant::from_std(end - Duration::from_secs(3)),
                exited(pid),
            )
            .await
            .is_ok_and(|result| result.is_ok());
            let retired = finish_before(pid, end).await;
            let status = group.reap().await.map_err(|_| Error::Close)?;
            require(natural && retired && !reserved(pid))?;
            if matches!(self.case, Case::Changed | Case::FirstCause) {
                fs::write(self.files.0.join("input"), b"changed original")
                    .map_err(|_| Error::Io)?;
            }
            if !status.success() || self.case == Case::FirstCause {
                return Err(Error::Command);
            }
            if self.case == Case::Close {
                // Inject a close-observer refusal only after an actual known close.
                let held = File::open(self.files.0.join("received")).map_err(|_| Error::Io)?;
                super::super::super::super::files::close(held, control)?;
                control.cleanup_failed = true;
                return Err(Error::Close);
            }
            control.check()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mixed_script_crosses_real_stdin_and_transport_or_original_failures_cannot_pass() {
        for case in [
            Case::Success,
            Case::Nonzero,
            Case::Changed,
            Case::Close,
            Case::FirstCause,
        ] {
            let files = Files::new();
            let mut control = Control::new_deploy(Instant::now()).unwrap();
            control
                .push_phase(Duration::from_secs(COMMAND_SECONDS))
                .unwrap();
            let end = control.phase_end();
            let mut source =
                Original::fixture(&files.0.join("input"), MAXIMUM, 0o600, &mut control).unwrap();
            // Positional original readers leave this shared offset irrelevant to admission.
            let mut held = source.file().unwrap();
            held.seek(SeekFrom::End(0)).unwrap();
            let mut transport = Inert {
                files: &files,
                case,
                calls: 0,
                seen: Vec::new(),
            };
            let result = execute(&source, &mut transport, &mut control, &"a".repeat(64)).await;
            let expected = match case {
                Case::Success => Ok(()),
                Case::Nonzero | Case::FirstCause => Err(Error::Command),
                Case::Changed => Err(Error::Changed),
                Case::Close => Err(Error::Close),
            };
            assert_eq!(result, expected);
            assert_eq!(control.first, expected.err());
            assert_eq!(transport.calls, 1);
            assert_eq!(transport.seen, arguments(&"a".repeat(64)).unwrap());
            assert!(!transport.seen.iter().any(|arg| arg == "--command"
                || arg == "-c"
                || arg.contains("GRANT")
                || arg.contains("__SC_GRANTS_STDIN_SENTINEL__")));
            assert_eq!(
                fs::read(files.0.join("received")).unwrap(),
                [SCRIPT, SENTINEL].concat()
            );
            control.begin_settling();
            let closed = source.finish(&mut control);
            assert_eq!(
                closed.err(),
                matches!(case, Case::Changed | Case::FirstCause).then_some(Error::Changed)
            );
            assert!(source.closed());
            assert_eq!(control.finish(), expected);
            files.remove().unwrap();
            assert!(Instant::now() < end);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_changed_retained_script_refuses_before_any_stdin_child() {
        let files = Files::new();
        let mut control = Control::new_deploy(Instant::now()).unwrap();
        let mut source =
            Original::fixture(&files.0.join("input"), MAXIMUM, 0o600, &mut control).unwrap();
        fs::write(files.0.join("input"), b"changed before transfer").unwrap();
        let mut transport = Inert {
            files: &files,
            case: Case::Success,
            calls: 0,
            seen: Vec::new(),
        };
        assert_eq!(
            execute(&source, &mut transport, &mut control, &"a".repeat(64)).await,
            Err(Error::Changed)
        );
        assert_eq!(transport.calls, 0);
        assert!(fs::read(files.0.join("received")).unwrap().is_empty());
        control.begin_settling();
        assert_eq!(source.finish(&mut control), Err(Error::Changed));
        assert!(source.closed());
        assert_eq!(control.finish(), Err(Error::Changed));
        files.remove().unwrap();
    }
}
