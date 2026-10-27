//! Owned fake material trees. One native std-only driver is compiled per test
//! process; its Cargo role writes inert ELF data and never runs a candidate.

use super::*;
use crate::ci::{command::Command, process::Executor};
use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
static DRIVER: OnceLock<Vec<u8>> = OnceLock::new();

#[path = "fixture_driver.rs"]
mod driver;
#[path = "fixture_driver_tests.rs"]
mod driver_tests;

struct Directory {
    path: PathBuf,
    owned: bool,
}
impl Directory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self { path, owned: true }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.owned {
            let removed = fs::remove_dir_all(&self.path);
            if !std::thread::panicking() {
                removed.unwrap();
            }
        }
    }
}

/// The owned worker avoids nesting a runtime when a Tokio test constructs a
/// fixture. Its compiler group is settled before joining or reading its output.
/// The watchdog does not bound blocked kernel/reap latency.
fn driver_bytes() -> &'static [u8] {
    DRIVER.get_or_init(|| {
        let directory = Directory::new("ops-producer-driver");
        let output = directory.path.join("driver");
        let log = directory.path.join("compiler.log");
        let worker_output = output.clone();
        let worker_log = log.clone();
        let compiled = std::thread::spawn(move || {
            use simplestchat_ops::owned_process::{Group, exited, finish_before};
            use std::{
                os::unix::process::CommandExt,
                process::{Command, Stdio},
            };

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let end = Instant::now() + Duration::from_secs(65);
                let compiler = std::env::var_os("RUSTC")
                    .unwrap_or_else(|| PathBuf::from(env!("CARGO")).with_file_name("rustc").into());
                let log = fs::File::create(worker_log)?;
                let child = Command::new(compiler)
                    .args([
                        "--edition=2024",
                        "--crate-name=ops_producer_fixture_driver",
                        "-Dwarnings",
                        "-Cstrip=symbols",
                        "-Cdebuginfo=0",
                    ])
                    .arg(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/src/ci/ops_artifact/fixture_driver.rs"
                    ))
                    .arg("-o")
                    .arg(worker_output)
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone()?)
                    .stderr(log)
                    .spawn()?;
                let mut group = Group::new(child);
                let pid = group.pid();
                let observed = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(end - Duration::from_secs(5)),
                    exited(pid),
                )
                .await;
                let retired = finish_before(pid, end).await;
                let reaped = group.reap().await;
                Ok::<_, std::io::Error>(
                    matches!(observed, Ok(Ok(())))
                        && retired
                        && reaped.is_ok_and(|status| status.success())
                        && Instant::now() < end,
                )
            })
        })
        .join()
        .unwrap()
        .unwrap();
        assert!(
            compiled,
            "fixture driver compilation: {}",
            String::from_utf8_lossy(&fs::read(&log).unwrap())
        );
        let bytes = fs::read(output).unwrap();
        assert!(!bytes.is_empty() && bytes.len() <= 16 * 1024 * 1024);
        bytes
    })
}
pub(super) struct FakeClock(pub Cell<Instant>);
impl FakeClock {
    pub fn new() -> Self {
        Self(Cell::new(Instant::now()))
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
pub(super) struct Noop;
impl Executor for Noop {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("unexpected fixture command")
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}
pub(super) fn binary() -> Vec<u8> {
    let mut bytes = vec![0u8; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    let u16_at = |bytes: &mut [u8], offset: usize, value: u16| {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes())
    };
    let u32_at = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
    };
    let u64_at = |bytes: &mut [u8], offset: usize, value: u64| {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes())
    };
    u16_at(&mut bytes, 16, 2);
    u16_at(&mut bytes, 18, 62);
    u32_at(&mut bytes, 20, 1);
    u64_at(&mut bytes, 24, 0x400040);
    u64_at(&mut bytes, 32, 64);
    u16_at(&mut bytes, 52, 64);
    u16_at(&mut bytes, 54, 56);
    u16_at(&mut bytes, 56, 2);
    u32_at(&mut bytes, 64, 1);
    u32_at(&mut bytes, 68, 5);
    u64_at(&mut bytes, 80, 0x400000);
    u64_at(&mut bytes, 96, 4096);
    u64_at(&mut bytes, 104, 4096);
    u64_at(&mut bytes, 112, 4096);
    u32_at(&mut bytes, 120, 0x6474e551);
    u32_at(&mut bytes, 124, 6);
    bytes
}
pub(super) struct Fixture {
    pub root: PathBuf,
    pub source: PathBuf,
    pub kit: PathBuf,
    pub output: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let mut directory = Directory::new("ops-producer-fixture");
        let root = directory.path.clone();
        let source = root.join("source");
        let kit = root.join("kit");
        let output = root.join("run");
        for path in [
            source.join("ops/src"),
            kit.join("toolchain/bin"),
            kit.join("host-tools/bin"),
            kit.join("toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin"),
            kit.join("toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib"),
            kit.join("registry"),
            kit.join("registry/openssl-0.10.81"),
            kit.join("registry/x509-parser-0.18.1"),
            kit.join("registry/asn1-rs-0.7.2"),
            kit.join("toolchain/fixture"),
            kit.join("native/musl"),
            kit.join("native/openssl"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(
            source.join("Cargo.toml"),
            b"[workspace]\nmembers=[\"ops\"]\n",
        )
        .unwrap();
        fs::write(source.join("Cargo.lock"), b"# fake fixture lockfile\n").unwrap();
        fs::write(
            source.join("ops/Cargo.toml"),
            b"[package]\nname=\"simplestchat-ops\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        fs::write(source.join("ops/src/main.rs"), b"// inert fake source\n").unwrap();
        let driver = driver_bytes();
        for path in [
            "toolchain/bin/cargo",
            "toolchain/bin/rustc",
            "toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld",
            "host-tools/bin/cc",
            "host-tools/bin/musl-cc",
        ] {
            fs::write(kit.join(path), driver).unwrap();
            fs::set_permissions(kit.join(path), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            kit.join("toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib/fake.rlib"),
            b"not a compiler library",
        )
        .unwrap();
        for (relative, bytes) in [
            (
                "native/musl/COPYRIGHT",
                b"Inert musl notice fixture\n".as_slice(),
            ),
            (
                "native/openssl/LICENSE.txt",
                b"Inert OpenSSL notice fixture\n".as_slice(),
            ),
            ("toolchain/fixture/build-status", b"0\n".as_slice()),
        ] {
            fs::write(kit.join(relative), bytes).unwrap();
        }
        for (name, version) in [
            ("openssl", "0.10.81"),
            ("x509-parser", "0.18.1"),
            ("asn1-rs", "0.7.2"),
        ] {
            fs::write(
                kit.join(format!("registry/{name}-{version}/Cargo.toml")),
                format!("[package]\nname=\"{name}\"\nversion=\"{version}\"\n"),
            )
            .unwrap();
        }
        fs::write(kit.join("toolchain/fixture/candidate.elf"), binary()).unwrap();
        let fixture = Self {
            root,
            source,
            kit,
            output,
        };
        fixture.set_metadata(&fixture.metadata());
        // Transfer the completed directory to Fixture's checked test teardown.
        directory.owned = false;
        fixture
    }
    pub fn metadata(&self) -> serde_json::Value {
        let registry = "registry+https://github.com/rust-lang/crates.io-index";
        serde_json::json!({"workspace_root":self.output.join("source"),
            "workspace_members":["fixture-ops"],"packages":[
                {"id":"fixture-ops","name":"simplestchat-ops","version":"0.1.0",
                    "manifest_path":self.output.join("source/ops/Cargo.toml"),"source":null},
                {"id":"fixture-openssl","name":"openssl","version":"0.10.81",
                    "manifest_path":self.output.join("kit/registry/openssl-0.10.81/Cargo.toml"),
                    "source":registry},
                {"id":"fixture-x509","name":"x509-parser","version":"0.18.1",
                    "manifest_path":self.output.join("kit/registry/x509-parser-0.18.1/Cargo.toml"),
                    "source":registry},
                {"id":"fixture-asn1","name":"asn1-rs","version":"0.7.2",
                    "manifest_path":self.output.join("kit/registry/asn1-rs-0.7.2/Cargo.toml"),
                    "source":registry}],"resolve":{"nodes":[
                {"id":"fixture-ops","features":["maintain-guest"],"deps":[
                    {"name":"openssl","pkg":"fixture-openssl"},
                    {"name":"x509_parser","pkg":"fixture-x509"},
                    {"name":"asn1_rs","pkg":"fixture-asn1"}]},
                {"id":"fixture-openssl","features":[],"deps":[]},
                {"id":"fixture-x509","features":[],"deps":[]},
                {"id":"fixture-asn1","features":["bigint"],"deps":[]}]}})
    }
    pub fn set_metadata(&self, value: &serde_json::Value) {
        fs::write(
            self.kit.join("toolchain/fixture/metadata.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }
    pub fn fail_build(&self) {
        fs::write(self.kit.join("toolchain/fixture/build-status"), b"100\n").unwrap();
    }
    pub fn selection(&self) -> serde_json::Value {
        serde_json::json!({"repository":"swiftraccoon/simplestChat","revision":"a".repeat(40),
            "packageVersion":"0.1.0","protocolVersion":1,"target":TARGET,"buildRunId":123,
            "runAttempt":1})
    }
    pub async fn request(&self, budget: &namespace::Budget) -> request::Request {
        let clock = NativeClock;
        let deadline = Deadline::new(&clock);
        let mut executor = Noop;
        let source = materials::measure(&self.source, budget, &mut executor, &deadline)
            .await
            .unwrap();
        let kit = materials::measure(&self.kit, budget, &mut executor, &deadline)
            .await
            .unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({"schemaVersion":1,
            "selection":self.selection(),"source":{"path":self.source,"sha256":source.sha256},
            "builderKit":{"path":self.kit,"sha256":kit.sha256},"output":self.output}))
        .unwrap();
        request::Request::decode(&bytes, &sha(&bytes)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
