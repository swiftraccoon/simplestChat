//! Owned fake material trees. Fake Cargo writes inert ELF metadata only; none
//! of these fixtures invokes a compiler, linker, network client or candidate.

use super::*;
use crate::ci::{command::Command, process::Executor};
use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
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
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-producer-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
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
        let script = format!(
            r#"#!/usr/bin/python3
import json,os,sys
from pathlib import Path
root=Path.cwd()
args=sys.argv[1:]
name=Path(sys.argv[0]).name
if name=='rustc':
    if args==['-vV']:
        print('rustc 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\nhost: x86_64-unknown-linux-gnu')
    elif args==['--print','sysroot']:
        print(root/'kit/toolchain')
    else:
        sys.exit(96)
elif name=='cargo' and args==['--version','--verbose']:
    print('cargo 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\nhost: x86_64-unknown-linux-gnu')
elif name=='cargo' and args[0]=='metadata':
    manifest=str(root/'source/ops/Cargo.toml')
    print(json.dumps({{'workspace_root':str(root/'source'),'workspace_members':['fixture-ops'],
        'packages':[{{'id':'fixture-ops','name':'simplestchat-ops','version':'0.1.0',
        'manifest_path':manifest,'source':None}}]}}))
elif name=='cargo' and args[0]=='build':
    target=root/'target/x86_64-unknown-linux-musl/release'
    target.mkdir(parents=True)
    candidate=target/'simplestchat-ops'
    candidate.write_bytes(bytes.fromhex('{}'))
    candidate.chmod(0o600)
    os.write(1,b'PRIVATE-STDOUT\n')
    os.write(2,b'PRIVATE-STDERR\n')
else:
    sys.exit(97)
"#,
            hex(&binary())
        );
        for path in [
            "toolchain/bin/cargo",
            "toolchain/bin/rustc",
            "toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld",
            "host-tools/bin/cc",
        ] {
            fs::write(kit.join(path), &script).unwrap();
            fs::set_permissions(kit.join(path), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            kit.join("toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib/fake.rlib"),
            b"not a compiler library",
        )
        .unwrap();
        Self {
            root,
            source,
            kit,
            output,
        }
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
